// Reed-Solomon over GF(256) (polynomial 0x11d, first root α^0), the code QR codes and ggwave use, ported from
// luchd's find/rs.rs: the sound modem's frames go through it, so a few tones lost to a clap are mended.
// SPDX-License-Identifier: GPL-3.0-or-later
import Foundation

enum ReedSolomon {
    private static let tables: (exp: [UInt8], log: [UInt8]) = {
        var exp = [UInt8](repeating: 0, count: 512)
        var log = [UInt8](repeating: 0, count: 256)
        var x: UInt16 = 1
        for i in 0..<255 {
            exp[i] = UInt8(x)
            log[Int(x)] = UInt8(i)
            x <<= 1
            if x & 0x100 != 0 { x ^= 0x11d }
        }
        for i in 255..<512 { exp[i] = exp[i - 255] }
        return (exp, log)
    }()

    static func mul(_ a: UInt8, _ b: UInt8) -> UInt8 {
        guard a != 0, b != 0 else { return 0 }
        return tables.exp[Int(tables.log[Int(a)]) + Int(tables.log[Int(b)])]
    }

    static func div(_ a: UInt8, _ b: UInt8) -> UInt8 {
        guard a != 0 else { return 0 }
        return tables.exp[(Int(tables.log[Int(a)]) + 255 - Int(tables.log[Int(b)])) % 255]
    }

    static func powAlpha(_ e: Int) -> UInt8 { tables.exp[e % 255] }

    /// The generator polynomial (highest degree first): (x - α^0)(x - α^1)...(x - α^(n-1)).
    static func generator(_ nsym: Int) -> [UInt8] {
        var g: [UInt8] = [1]
        for i in 0..<nsym {
            let root = powAlpha(i)
            var next = [UInt8](repeating: 0, count: g.count + 1)
            for (j, c) in g.enumerated() {
                next[j] ^= c
                next[j + 1] ^= mul(c, root)
            }
            g = next
        }
        return g
    }

    /// The message followed by `nsym` parity bytes (the sender only listens: the tests make their frames with it).
    static func encode(_ msg: [UInt8], _ nsym: Int) -> [UInt8] {
        precondition(msg.count + nsym <= 255, "a Reed-Solomon block holds 255 bytes")
        let g = generator(nsym)
        var rem = [UInt8](repeating: 0, count: nsym)
        for m in msg {
            let factor = m ^ rem[0]
            rem.removeFirst()
            rem.append(0)
            for i in 0..<nsym { rem[i] ^= mul(g[i + 1], factor) }
        }
        return msg + rem
    }

    static func syndromes(_ word: [UInt8], _ nsym: Int) -> [UInt8] {
        (0..<nsym).map { j in
            let a = powAlpha(j)
            return word.reduce(0) { mul($0, a) ^ $1 }
        }
    }

    /// Corrects up to `nsym / 2` wrong bytes in place and returns how many there were, or nil when there are more.
    static func correct(_ word: inout [UInt8], _ nsym: Int) -> Int? {
        let s = syndromes(word, nsym)
        if s.allSatisfy({ $0 == 0 }) { return 0 }
        // Berlekamp-Massey: the error locator Λ, lowest degree first.
        var c: [UInt8] = [1], b: [UInt8] = [1]
        var l = 0, m = 1
        var bd: UInt8 = 1
        for n in 0..<nsym {
            var d = s[n]
            let upto = min(l, c.count - 1)
            if upto >= 1 {
                for i in 1...upto { d ^= mul(c[i], s[n - i]) }
            }
            if d == 0 {
                m += 1
                continue
            }
            let coef = div(d, bd)
            var next = c
            if next.count < b.count + m { next += [UInt8](repeating: 0, count: b.count + m - next.count) }
            for (i, bc) in b.enumerated() { next[i + m] ^= mul(coef, bc) }
            if 2 * l <= n {
                b = c
                l = n + 1 - l
                bd = d
                m = 1
            } else {
                m += 1
            }
            c = next
        }
        guard l != 0, 2 * l <= nsym else { return nil }
        let n = word.count
        func eval(_ p: [UInt8], _ x: UInt8) -> UInt8 { p.reversed().reduce(0) { mul($0, x) ^ $1 } }
        // Chien search: position i is the coefficient of x^(n-1-i); Λ has a root at X^-1 for each wrong one.
        let positions = (0..<n).filter { i in eval(c, powAlpha(255 - (n - 1 - i) % 255)) == 0 }
        guard positions.count == l else { return nil }
        // Forney: Ω = S·Λ mod x^nsym; e = X·Ω(X^-1) / Λ'(X^-1).
        var omega = [UInt8](repeating: 0, count: nsym)
        for (i, si) in s.enumerated() {
            for (j, cj) in c.enumerated() where i + j < nsym { omega[i + j] ^= mul(si, cj) }
        }
        let deriv: [UInt8] = c.count > 1 ? (1..<c.count).map { $0 % 2 == 1 ? c[$0] : 0 } : []
        for i in positions {
            let power = (n - 1 - i) % 255
            let x = powAlpha(power)
            let xInv = powAlpha(255 - power)
            let denom = eval(deriv, xInv)
            guard denom != 0 else { return nil }
            word[i] ^= mul(x, div(eval(omega, xInv), denom))
        }
        // A word too damaged can look correctable: only a clean result counts.
        return syndromes(word, nsym).allSatisfy { $0 == 0 } ? positions.count : nil
    }
}
