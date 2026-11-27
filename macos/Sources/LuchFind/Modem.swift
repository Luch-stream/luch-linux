// Data over sound (PROTOCOL.md 1d), ported from luchd's find/modem.rs: a receiver says its card as a short run of
// tones near the top of what people hear, and this Mac listens with its microphone. 16-tone FSK (4 bits per 40 ms
// symbol), a fixed preamble to find the frame, Reed-Solomon to mend it.
//
// Pure code: samples in, bytes out. The encoder is here too, though a sender only listens: the tests make their
// frames with it, and its tones must be the receiver's (the same golden vector as luchd and Chirp.java).
// SPDX-License-Identifier: GPL-3.0-or-later
import Foundation

public enum Modem {
    public static let rate = 48_000
    /// 40 ms: long enough for a room's echo to fade under the next tone, short enough for a 2 s frame.
    public static let symbol = 1920
    public static let tones = 16
    /// 100 Hz between tones, three times the analysis window's resolution.
    public static let spacing: Float = 100
    /// Tone 0 of each band: near-ultrasonic first, the audible band as the fallback.
    public static let bands: [Float] = [17_000, 1_800]
    /// All different: shifted by a symbol or more, the preamble matches nothing of itself.
    public static let preamble: [UInt8] = [0, 15, 3, 12, 6, 9, 1, 14]
    public static let parity = 8
    public static let maxPayload = 32
    /// Raised-cosine edges of every tone (2 ms), so symbol changes do not click.
    static let ramp = 96
    /// The part of a symbol that is analysed: its middle three quarters.
    static let winStart = symbol / 8
    static let winLen = symbol * 3 / 4

    /// Tone indices of a frame: preamble, then the Reed-Solomon codeword of `[len, payload..]`, high nibble first.
    public static func symbols(_ payload: [UInt8]) -> [UInt8] {
        precondition(payload.count <= maxPayload)
        let word = ReedSolomon.encode([UInt8(payload.count)] + payload, parity)
        return preamble + word.flatMap { [$0 >> 4, $0 & 15] }
    }

    /// The frame as samples at `rate` (the receiver's output rate), peak `amp`.
    public static func synth(_ payload: [UInt8], band: Float, amp: Float, rate: Int = Modem.rate) -> [Float] {
        let per = symbol * rate / Modem.rate
        let r = ramp * rate / Modem.rate
        var out: [Float] = []
        out.reserveCapacity(per * symbols(payload).count)
        for s in symbols(payload) {
            let w = 2 * Double.pi * Double(band + Float(s) * spacing) / Double(rate)
            for i in 0..<per {
                let edge = min(i, per - 1 - i)
                let env = edge < r ? 0.5 - 0.5 * cos(Double.pi * Double(edge) / Double(r)) : 1
                out.append(Float(Double(amp) * env * sin(w * Double(i))))
            }
        }
        return out
    }

    static let hann: [Float] = (0..<winLen).map { 0.5 - 0.5 * cos(2 * Float.pi * Float($0) / Float(winLen - 1)) }

    /// Energy of each tone in the window of the symbol starting at `start` (Goertzel, Hann-windowed).
    static func energies(_ x: UnsafeBufferPointer<Float>, _ start: Int, _ band: Float) -> [Float]? {
        let from = start + winStart
        guard from >= 0, from + winLen <= x.count else { return nil }
        var e = [Float](repeating: 0, count: tones)
        hann.withUnsafeBufferPointer { h in
            for k in 0..<tones {
                let w = 2 * Float.pi * (band + Float(k) * spacing) / Float(rate)
                let coeff = 2 * cos(w)
                var s1: Float = 0, s2: Float = 0
                for i in 0..<winLen {
                    let s0 = x[from + i] * h[i] + coeff * s1 - s2
                    s2 = s1
                    s1 = s0
                }
                e[k] = s1 * s1 + s2 * s2 - coeff * s1 * s2
            }
        }
        return e
    }

    /// How much of the energy is in `tone`: 1 for a clean tone, about 1/16 for noise.
    static func share(_ e: [Float], _ tone: UInt8) -> Float {
        let sum = e.reduce(0, +)
        return sum <= 0 ? 0 : e[Int(tone)] / sum
    }

    static func best(_ e: [Float]) -> (UInt8, Float) {
        var k = 0
        for i in 1..<e.count where e[i] > e[k] { k = i }
        return (UInt8(k), share(e, UInt8(k)))
    }

    /// Where a preamble starts, best first: a coarse search on an eighth of a symbol, then a fine one around each hit.
    static func preambles(_ x: UnsafeBufferPointer<Float>, _ band: Float) -> [Int] {
        let step = symbol / 8
        guard x.count >= preamble.count * symbol else { return [] }
        // Energies at every grid point once: the preamble's symbols fall on the same grid, 8 points apart.
        let grid: [[Float]?] = (0...((x.count - winStart - winLen) / step)).map { energies(x, $0 * step, band) }
        let perSymbol = symbol / step
        func scoreAt(_ g: Int) -> Float {
            var sum: Float = 0
            for (i, t) in preamble.enumerated() {
                let at = g + i * perSymbol
                if at < grid.count, let e = grid[at] { sum += share(e, t) }
            }
            return sum / Float(preamble.count)
        }
        let scores = (0..<grid.count).map(scoreAt)
        func scoreFine(_ t: Int) -> Float {
            var sum: Float = 0
            for (i, tone) in preamble.enumerated() {
                if let e = energies(x, t + i * symbol, band) { sum += share(e, tone) }
            }
            return sum / Float(preamble.count)
        }
        // Peaks: the best score within a symbol either side.
        var hits: [(Int, Float)] = []
        for (g, s) in scores.enumerated() where s > 0.5 {
            let near = max(0, g - perSymbol)..<min(g + perSymbol + 1, scores.count)
            guard near.allSatisfy({ scores[$0] < s || (scores[$0] == s && $0 >= g) }) else { continue }
            let coarse = g * step
            var fine = (coarse, -Float.greatestFiniteMagnitude)
            for d in stride(from: -step, through: step, by: 16) {
                let f = scoreFine(coarse + d)
                if f > fine.1 { fine = (coarse + d, f) }
            }
            hits.append(fine)
        }
        return hits.sorted { $0.1 > $1.1 }.map(\.0)
    }

    /// Reads the frame whose preamble starts at `t0`, following a sample clock that runs a little fast or slow.
    static func frame(_ x: UnsafeBufferPointer<Float>, at t0: Int, _ band: Float) -> [UInt8]? {
        let nudge = symbol / 64
        let most = 2 * (1 + maxPayload + parity)
        var offset = 0
        var got: [UInt8] = []
        for k in 0..<most {
            let at = t0 + (preamble.count + k) * symbol + offset
            var choice: (d: Int, tone: UInt8, s: Float)?
            for d in [0, -nudge, nudge] {
                guard let e = energies(x, at + d, band) else { continue }
                let (tone, s0) = best(e)
                // Staying put wins ties: noise must not walk the clock away.
                let s = d == 0 ? s0 * 1.05 : s0
                if choice == nil || s > choice!.s { choice = (d, tone, s) }
            }
            guard let c = choice else { break }
            offset += c.d
            got.append(c.tone)
        }
        let bytes = stride(from: 0, to: got.count - 1, by: 2).map { got[$0] << 4 | got[$0 + 1] }
        for len in 0...maxPayload {
            let n = len + 1 + parity
            guard n <= bytes.count else { break }
            var word = Array(bytes[0..<n])
            if ReedSolomon.correct(&word, parity) != nil, Int(word[0]) == len {
                return Array(word[1..<1 + len])
            }
        }
        return nil
    }

    /// Every frame heard in `x` (both bands), each once.
    public static func demodulate(_ samples: [Float]) -> [[UInt8]] {
        samples.withUnsafeBufferPointer { x in
            var out: [[UInt8]] = []
            for band in bands {
                for t0 in preambles(x, band) {
                    if let p = frame(x, at: t0, band), !out.contains(p) { out.append(p) }
                }
            }
            return out
        }
    }
}
