// The acoustic calibration's signal and its detector (docs/research/sync-multiroom.md 6.7): a port of the receiver's
// Probe.java and Fft.java (luch-android ru.toxblh.luch.sync), so that the Mac's microphone is read exactly as the
// receiver's simulator proved it. Pure code: tested on recordings synthesised in memory.
// SPDX-License-Identifier: GPL-3.0-or-later
import Foundation

/// The probe is a logarithmic sweep from 2 to 6 kHz: above most of the energy of fans and mains hum, reproduced by
/// every small speaker, and compressed by its matched filter into a pulse about 0.25 ms wide. Per sweep, the recording
/// around its expected time is correlated with the sweep in 1.5–7 kHz, as the envelope of the analytic correlation (so
/// the pulse's phase, which a reflection or a speaker's polarity turns, does not matter).
///
/// The direct path is the earliest strong peak, not the largest one: reflections come later and can be louder. It is
/// the first local maximum of at least a third of the largest within 50 ms before it, timed to a fraction of a sample
/// by a parabola. It is trusted when it stands at least 12 dB over everything before it (only noise comes before the
/// direct sound), and a device is measured when all its sweeps are trusted and agree within 0.5 ms.
public final class AcousticProbe {
    public static let f0 = 2000.0, f1 = 6000.0, ms = 200.0
    public static let bandLo = 1500.0, bandHi = 7000.0
    public static let minPsrDb = 12.0, agreeMs = 0.5
    /// The direct path: the earliest peak of at least this share of the largest, at most this long before it.
    static let directShare = 1 / 3.0, directBeforeMs = 50.0
    /// What counts as "before the direct sound" for the sidelobes: from the window's start to this long before it.
    static let guardMs = 1.0, minLeadMs = 20.0

    public let rate: Int
    private let sweep: [Float]
    private var fft: Fft?
    private var sweepRe: [Double] = []
    private var sweepIm: [Double] = []

    public init(rate: Int) {
        self.rate = rate
        sweep = Self.sweep(rate: rate, f0: Self.f0, f1: Self.f1, ms: Self.ms, gain: 1)
    }

    /// A logarithmic sweep f0 → f1 of `ms`, peak `gain`, with 5 ms raised-cosine fades so that it starts and ends
    /// without a click.
    public static func sweep(rate: Int, f0: Double, f1: Double, ms: Double, gain: Double) -> [Float] {
        let n = Int((Double(rate) * ms / 1000).rounded())
        let t1 = ms / 1000, k = log(f1 / f0)
        let fade = rate * 5 / 1000
        return (0..<n).map { i in
            let t = Double(i) / Double(rate)
            let phase = 2 * Double.pi * f0 * t1 / k * (exp(t / t1 * k) - 1)
            var g = gain
            if i < fade {
                g *= 0.5 - 0.5 * cos(Double.pi * Double(i) / Double(fade))
            } else if i >= n - fade {
                g *= 0.5 - 0.5 * cos(Double.pi * Double(n - 1 - i) / Double(fade))
            }
            return Float(g * sin(phase))
        }
    }

    /// One sweep's arrival: where its first sample was heard (fractional sample), and how sure the detector is.
    public struct Arrival {
        public let at: Double
        public let psrDb: Double
        public let trusted: Bool
        public let why: String
    }

    /// A device's measurement: the median of (heard − scheduled), in samples, when it passed. `arrivals` stops at the
    /// first sweep not trusted.
    public struct Result {
        public let accepted: Bool
        public let delaySamples: Double
        public let arrivals: [Arrival]
        public let why: String

        public func delayMs(rate: Int) -> Double { delaySamples * 1000 / Double(rate) }
    }

    /// Measures one device: its sweeps were scheduled to start at `scheduled` (samples of the recording's own
    /// timeline, its capture timestamps mapped to the sender's clock); each is looked for within ±`windowMs`.
    public func measure(_ rec: [Float], scheduled: [Double], windowMs: Double) -> Result {
        let w = Int((Double(rate) * windowMs / 1000).rounded())
        var a: [Arrival] = []
        var d: [Double] = []
        for (i, s) in scheduled.enumerated() {
            let one = find(rec, from: Int(s.rounded(.down)) - w, to: Int(s.rounded(.up)) + w)
            a.append(one)
            d.append(one.at - s)
            if !one.trusted { return Result(accepted: false, delaySamples: .nan, arrivals: a, why: "sweep \(i): \(one.why)") }
        }
        guard !d.isEmpty else { return Result(accepted: false, delaySamples: .nan, arrivals: a, why: "no sweeps") }
        let s = d.sorted()
        let spread = (s[s.count - 1] - s[0]) * 1000 / Double(rate)
        if spread > Self.agreeMs {
            return Result(accepted: false, delaySamples: .nan, arrivals: a,
                          why: String(format: "the sweeps disagree by %.2f ms", spread))
        }
        let n = s.count
        let median = n % 2 == 1 ? s[n / 2] : (s[n / 2 - 1] + s[n / 2]) / 2
        return Result(accepted: true, delaySamples: median, arrivals: a, why: "ok")
    }

    /// Where the sweep starts in `rec`, looked for at starts from..to (samples).
    public func find(_ rec: [Float], from: Int, to: Int) -> Arrival {
        let m = sweep.count, span = to - from
        if from < 0 || to + m > rec.count || span <= 0 {
            return Arrival(at: .nan, psrDb: 0, trusted: false, why: "outside the recording")
        }
        let len = span + m
        let size = 1 << (Int.bitWidth - (len - 1).leadingZeroBitCount)
        prepare(size)
        guard let fft else { return Arrival(at: .nan, psrDb: 0, trusted: false, why: "no transform") }
        var re = [Double](repeating: 0, count: size), im = [Double](repeating: 0, count: size)
        for i in 0..<len { re[i] = Double(rec[from + i]) }
        fft.transform(&re, &im, inverse: false)
        // The analytic correlation in the band: X · conj(S) on positive frequencies only, doubled.
        let lo = Int((Self.bandLo * Double(size) / Double(rate)).rounded(.up))
        let hi = Int((Self.bandHi * Double(size) / Double(rate)).rounded(.down))
        for k in 0..<size {
            if k < lo || k > hi {
                re[k] = 0
                im[k] = 0
                continue
            }
            let xr = re[k], xi = im[k], sr = sweepRe[k], si = -sweepIm[k]
            re[k] = 2 * (xr * sr - xi * si)
            im[k] = 2 * (xr * si + xi * sr)
        }
        fft.transform(&re, &im, inverse: true)
        var env = [Double](repeating: 0, count: span + 1)
        var peak = 0
        for k in 0...span {
            env[k] = hypot(re[k], im[k])
            if env[k] > env[peak] { peak = k }
        }
        if env[peak] <= 0 { return Arrival(at: .nan, psrDb: 0, trusted: false, why: "silence") }
        // The direct path: the earliest local maximum of at least a third of the largest within 50 ms before it.
        let before = Int(Double(rate) * Self.directBeforeMs / 1000)
        let first = max(1, peak - before)
        let direct = first < peak ? (first..<peak).first {
            env[$0] >= Self.directShare * env[peak] && env[$0] >= env[$0 - 1] && env[$0] >= env[$0 + 1]
        } ?? peak : peak
        // Only noise comes before the direct sound: the sweep must stand 12 dB over all of it.
        let guardSamples = Int((Double(rate) * Self.guardMs / 1000).rounded(.up))
        let lead = Int(Double(rate) * Self.minLeadMs / 1000)
        if direct - guardSamples < lead {
            return Arrival(at: Double(from + direct), psrDb: 0, trusted: false,
                           why: "too close to the window's start: too early to tell")
        }
        var side = 0.0
        for k in 0..<(direct - guardSamples) { side = max(side, env[k]) }
        let psr = 20 * log10(env[direct] / max(side, 1e-30))
        let at = Double(from + direct) + Self.parabola(env, direct)
        let ok = psr >= Self.minPsrDb
        return Arrival(at: at, psrDb: psr, trusted: ok, why: ok ? "ok" : String(format: "peak only %.1f dB over noise", psr))
    }

    /// The vertex of the parabola through a peak and its neighbours, as an offset from the peak (−0.5..0.5).
    private static func parabola(_ v: [Double], _ i: Int) -> Double {
        if i <= 0 || i >= v.count - 1 { return 0 }
        let a = v[i - 1], b = v[i], c = v[i + 1]
        let den = a - 2 * b + c
        return den == 0 ? 0 : max(-0.5, min(0.5, 0.5 * (a - c) / den))
    }

    private func prepare(_ size: Int) {
        if fft?.size == size { return }
        let f = Fft(size)
        fft = f
        sweepRe = [Double](repeating: 0, count: size)
        sweepIm = [Double](repeating: 0, count: size)
        for (i, v) in sweep.enumerated() { sweepRe[i] = Double(v) }
        f.transform(&sweepRe, &sweepIm, inverse: false)
    }
}

/// An in-place radix-2 FFT of a power-of-two size, on separate real and imaginary arrays (Fft.java).
public final class Fft {
    public let size: Int
    private let cosT: [Double]
    private let sinT: [Double]
    private let rev: [Int]

    public init(_ n: Int) {
        precondition(n > 1 && n & (n - 1) == 0, "not a power of two: \(n)")
        size = n
        cosT = (0..<n / 2).map { cos(2 * Double.pi * Double($0) / Double(n)) }
        sinT = (0..<n / 2).map { sin(2 * Double.pi * Double($0) / Double(n)) }
        let bits = n.trailingZeroBitCount
        rev = (0..<n).map { i in (0..<bits).reduce(0) { $0 << 1 | (i >> $1) & 1 } }
    }

    /// Forward (inverse = false) or inverse transform; the inverse divides by n.
    public func transform(_ re: inout [Double], _ im: inout [Double], inverse: Bool) {
        let n = size
        let sign = inverse ? 1.0 : -1.0
        // Raw buffers: the bounds checks of a debug build would otherwise cost more than the arithmetic.
        re.withUnsafeMutableBufferPointer { re in
            im.withUnsafeMutableBufferPointer { im in
                cosT.withUnsafeBufferPointer { cosT in
                    sinT.withUnsafeBufferPointer { sinT in
                        rev.withUnsafeBufferPointer { rev in
                            for i in 0..<n {
                                let j = rev[i]
                                if j > i {
                                    re.swapAt(i, j)
                                    im.swapAt(i, j)
                                }
                            }
                            var len = 2
                            while len <= n {
                                let half = len >> 1, step = n / len
                                var i = 0
                                while i < n {
                                    for k in 0..<half {
                                        let wr = cosT[k * step], wi = sign * sinT[k * step]
                                        let a = i + k, b = a + half
                                        let xr = re[b] * wr - im[b] * wi, xi = re[b] * wi + im[b] * wr
                                        re[b] = re[a] - xr
                                        im[b] = im[a] - xi
                                        re[a] += xr
                                        im[a] += xi
                                    }
                                    i += len
                                }
                                len <<= 1
                            }
                            if inverse {
                                let d = Double(n)
                                for i in 0..<n {
                                    re[i] /= d
                                    im[i] /= d
                                }
                            }
                        }
                    }
                }
            }
        }
    }
}
