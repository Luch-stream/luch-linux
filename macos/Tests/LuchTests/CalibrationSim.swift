// The receiver's calibration simulator (luch-android SimRoom.java, DryRun.java), ported with java.util.Random so that
// the same seeds make the same rooms, and the Java tests' numbers can be checked here.
// SPDX-License-Identifier: GPL-3.0-or-later
import Foundation
@testable import LuchEngine

/// java.util.Random, shared as Java shares it (a room and its test draw from one): its 48-bit LCG, nextDouble, nextBoolean, nextInt(bound) and the polar nextGaussian.
final class JavaRandom {
    private var seed: Int64
    private var nextGaussianValue = 0.0
    private var haveNextGaussian = false

    init(_ seed: Int64) {
        self.seed = (seed ^ 0x5DEECE66D) & ((1 << 48) - 1)
    }

    private func next(_ bits: Int) -> Int32 {
        seed = (seed &* 0x5DEECE66D &+ 0xB) & ((1 << 48) - 1)
        return Int32(truncatingIfNeeded: seed >> (48 - bits))
    }

    func nextDouble() -> Double {
        let hi = Int64(next(26)) << 27
        return Double(hi + Int64(next(27))) * 0x1.0p-53
    }

    func nextBoolean() -> Bool { next(1) != 0 }

    func nextInt(_ bound: Int32) -> Int32 {
        var r = next(31)
        let m = bound - 1
        if bound & m == 0 { return Int32(truncatingIfNeeded: (Int64(bound) * Int64(r)) >> 31) }
        var u = r
        while true {
            r = u % bound
            if u &- r &+ m >= 0 { return r }
            u = next(31)
        }
    }

    func nextGaussian() -> Double {
        if haveNextGaussian {
            haveNextGaussian = false
            return nextGaussianValue
        }
        var v1 = 0.0, v2 = 0.0, s = 0.0
        repeat {
            v1 = 2 * nextDouble() - 1
            v2 = 2 * nextDouble() - 1
            s = v1 * v1 + v2 * v2
        } while s >= 1 || s == 0
        let multiplier = (-2 * log(s) / s).squareRoot()
        nextGaussianValue = v2 * multiplier
        haveNextGaussian = true
        return v1 * multiplier
    }
}

/// Java's Math.round for the positive numbers used here.
func javaRound(_ x: Double) -> Int { Int((x + 0.5).rounded(.down)) }

/// What one microphone records while one device plays its sweeps: the direct path and early reflections at their
/// exact (fractional) delays, a late reverberation, fans and hum at a chosen SNR, sometimes clipping.
final class SimRoom {
    static let rate = 48000
    let r: JavaRandom
    let distanceM, rt60, directToReverbDb: Double
    var reflDelay: [Double]
    var reflGain: [Double]
    let directGain: Double

    init(_ r: JavaRandom) {
        self.r = r
        distanceM = 1 + 5 * r.nextDouble()
        rt60 = 0.3 + 0.5 * r.nextDouble()
        directToReverbDb = -5 + 15 * r.nextDouble()
        directGain = 1 / distanceM
        let n = 3 + Int(r.nextInt(6))
        var d: [Double] = [], g: [Double] = []
        for _ in 0..<n {
            d.append(0.0005 + 0.0195 * r.nextDouble())
            // Up to 6 dB stronger than the direct path, either polarity.
            let sign: Double = r.nextBoolean() ? 1 : -1
            g.append(sign * pow(10, (-12 + 18 * r.nextDouble()) / 20))
        }
        reflDelay = d
        reflGain = g
    }

    /// The sweep, continuous: its value at t seconds after its start (0 outside it).
    static func sweepAt(_ t: Double) -> Double {
        let t1 = AcousticProbe.ms / 1000, k = log(AcousticProbe.f1 / AcousticProbe.f0), fade = 0.005
        if t < 0 || t >= t1 { return 0 }
        var g = 1.0
        if t < fade {
            g = 0.5 - 0.5 * cos(Double.pi * t / fade)
        } else if t > t1 - fade {
            g = 0.5 - 0.5 * cos(Double.pi * (t1 - t) / fade)
        }
        return g * sin(2 * Double.pi * AcousticProbe.f0 * t1 / k * (exp(t / t1 * k) - 1))
    }

    /// The room's response to one sweep, from when the direct sound arrives `fraction` of a sample after sample 0.
    func response(fraction: Double, gain: Double, fft: Fft) -> [Float] {
        let rate = Self.rate
        let m = javaRound(Double(rate) * AcousticProbe.ms / 1000), tail = Int(0.3 * Double(rate))
        let len = m + tail
        var y = [Double](repeating: 0, count: len)
        for i in 0..<len {
            let t = (Double(i) - fraction) / Double(rate)
            var v = directGain * Self.sweepAt(t)
            for k in reflDelay.indices { v += directGain * reflGain[k] * Self.sweepAt(t - reflDelay[k]) }
            y[i] = gain * v
        }
        let n = fft.size
        var hr = [Double](repeating: 0, count: n), hi = [Double](repeating: 0, count: n)
        var energy = 0.0
        let start = rate * 5 / 1000
        for i in start..<tail {
            let t = Double(i - start) / Double(rate)
            hr[i] = r.nextGaussian() * pow(10, -3 * t / rt60)
            energy += hr[i] * hr[i]
        }
        let scale = directGain * pow(10, -directToReverbDb / 20) / energy.squareRoot()
        var sr = [Double](repeating: 0, count: n), si = [Double](repeating: 0, count: n)
        for (i, v) in AcousticProbe.sweep(rate: rate, f0: AcousticProbe.f0, f1: AcousticProbe.f1, ms: AcousticProbe.ms,
                                          gain: 1).enumerated() { sr[i] = Double(v) }
        fft.transform(&hr, &hi, inverse: false)
        fft.transform(&sr, &si, inverse: false)
        for k in 0..<n {
            let a = hr[k] * sr[k] - hi[k] * si[k], b = hr[k] * si[k] + hi[k] * sr[k]
            hr[k] = a
            hi[k] = b
        }
        fft.transform(&hr, &hi, inverse: true)
        return (0..<len).map { Float(y[$0] + gain * scale * hr[$0]) }
    }

    /// A recording of `samples`, the response added where each sweep's direct sound arrives.
    static func place(_ samples: Int, _ response: [Float], at: [Int]) -> [Float] {
        var rec = [Float](repeating: 0, count: samples)
        for a in at {
            for i in response.indices where a + i < samples && a + i >= 0 { rec[a + i] += response[i] }
        }
        return rec
    }

    /// Fans: pink noise and 120 Hz hum with its harmonics, at `snrDb` below the direct sweep's power.
    func addFans(_ rec: inout [Float], snrDb: Double, sweepGain: Double) {
        let rate = Double(Self.rate)
        let signal = sq(sweepGain * directGain) / 2
        var noise = [Double](repeating: 0, count: rec.count)
        var b0 = 0.0, b1 = 0.0, b2 = 0.0, b3 = 0.0, b4 = 0.0, b5 = 0.0, b6 = 0.0, power = 0.0
        var phase = [Double](repeating: 0, count: 8)
        for h in phase.indices { phase[h] = 2 * Double.pi * r.nextDouble() }
        let humShare = 0.3 + 0.6 * r.nextDouble()
        for i in rec.indices {
            // Paul Kellet's pink filter.
            let w = r.nextGaussian()
            b0 = 0.99886 * b0 + w * 0.0555179
            b1 = 0.99332 * b1 + w * 0.0750759
            b2 = 0.96900 * b2 + w * 0.1538520
            b3 = 0.86650 * b3 + w * 0.3104856
            b4 = 0.55000 * b4 + w * 0.5329522
            b5 = -0.7616 * b5 - w * 0.0168980
            let pink = (b0 + b1 + b2 + b3 + b4 + b5 + b6 + w * 0.5362) * 0.11
            b6 = w * 0.115926
            var hum = 0.0
            for h in 1...phase.count {
                hum += sin(2 * Double.pi * 120 * Double(h) * Double(i) / rate + phase[h - 1]) / Double(h)
            }
            noise[i] = pink + humShare * 2 * hum
            power += noise[i] * noise[i]
        }
        power /= Double(rec.count)
        let scale = (signal / power / pow(10, snrDb / 10)).squareRoot()
        for i in rec.indices { rec[i] += Float(scale * noise[i]) }
    }

    /// White noise over the whole band, the sweep's included, at `snrDb`.
    func addWhite(_ rec: inout [Float], snrDb: Double, sweepGain: Double) {
        let signal = sq(sweepGain * directGain) / 2
        let scale = (signal / pow(10, snrDb / 10)).squareRoot()
        for i in rec.indices { rec[i] += Float(scale * r.nextGaussian()) }
    }

    /// A microphone overdriven: everything beyond `level` of the peak is cut off.
    static func clip(_ rec: inout [Float], level: Double) {
        let peak = rec.reduce(0.0) { max($0, Double(abs($1))) }
        let c = Float(peak * level)
        for i in rec.indices { rec[i] = max(-c, min(c, rec[i])) }
    }

    private func sq(_ x: Double) -> Double { x * x }
}

/// DryRun.java: four sweeps after a random delay, with a direct path, three reflections (one 6 dB louder) and noise.
enum DryRun {
    static let rate = 48000, sweeps = 4

    static func run(seed: Int64) -> (accepted: Bool, trueMs: Double, measuredMs: Double, psrDb: Double, why: String) {
        let r = JavaRandom(seed)
        let delay = 0.01 + 0.09 * r.nextDouble()
        let s = AcousticProbe.sweep(rate: rate, f0: AcousticProbe.f0, f1: AcousticProbe.f1, ms: AcousticProbe.ms, gain: 0.3)
        let gap = Int(0.7 * Double(rate)), lead = rate / 2
        var rec = [Float](repeating: 0, count: lead + sweeps * gap + rate)
        var sch: [Double] = []
        let paths: [(Double, Double)] = [(0, 1), (0.003, -0.6), (0.012, 2), (0.030, 0.5)]
        for k in 0..<sweeps {
            sch.append(Double(lead + k * gap))
            for p in paths {
                let at = javaRound(sch[k] + (delay + p.0) * Double(rate))
                for i in s.indices { rec[at + i] += Float(p.1 * Double(s[i])) }
            }
        }
        for i in rec.indices { rec[i] += Float(0.1 * r.nextGaussian()) }
        let res = AcousticProbe(rate: rate).measure(rec, scheduled: sch, windowMs: 150)
        let psr = res.arrivals.map(\.psrDb).min() ?? .greatestFiniteMagnitude
        let trueMs = Double(javaRound(delay * Double(rate))) * 1000 / Double(rate)
        return (res.accepted, trueMs, res.accepted ? res.delayMs(rate: rate) : .nan, psr, res.why)
    }
}

/// Runs `n` trials on all cores, each worker with its own detector and transform (they keep buffers).
func parallelTrials<T>(_ n: Int, _ trial: (Int, AcousticProbe, Fft) -> T) -> [T] {
    let workers = max(1, ProcessInfo.processInfo.activeProcessorCount)
    var out = [T?](repeating: nil, count: n)
    let lock = NSLock()
    withoutActuallyEscaping(trial) { trial in
        DispatchQueue.concurrentPerform(iterations: workers) { w in
            let probe = AcousticProbe(rate: SimRoom.rate), fft = Fft(32768)
            for i in stride(from: w, to: n, by: workers) {
                let t = trial(i, probe, fft)
                lock.lock()
                out[i] = t
                lock.unlock()
            }
        }
    }
    return out.map { $0! }
}
