// The latency lab's pattern (PROTOCOL.md 4j): a QR code of what this end knows about itself over two rows of
// Gray-code cells that count its refreshes. The reference is luch-linux luch-latency/src/pattern.rs; the receiver's
// QrCode.java makes the same choices, so all three draw the very same code for the same text.
// SPDX-License-Identifier: GPL-3.0-or-later
import Foundation

public enum LatencyPattern {
    /// The white box around the pattern: left, top, right, bottom, in units of the QR code's side.
    public static let box: [Double] = [-0.15, -0.15, 1.55, 2.2]
    /// Top and bottom of each row of cells.
    public static let rows: [[Double]] = [[1.15, 1.55], [1.65, 2.05]]
    public static let perRow = 7
    public static let cellW = 0.2
    /// Gray-code bits of the counter.
    public static let bits = 12
    /// The sender's QR code side and margin, in screen heights.
    public static let senderSide = 0.38
    public static let margin = 0.03

    /// Where the sender's pattern origin (its QR code's top-left corner) goes, in screen heights from the top left.
    public static var senderOrigin: [Double] { [margin - box[0] * senderSide, margin - box[1] * senderSide] }

    /// The layout as the page draws it: it has no numbers of its own.
    public static var layout: [String: Any] {
        ["box": box, "rows": rows, "per_row": perRow, "cell_w": cellW, "bits": bits, "side": senderSide,
         "origin": senderOrigin]
    }

    /// What a QR code says: `LUCH1;S;key=value;...`; empty values are left out and no value holds `;`.
    public static func text(role: String, _ fields: [(String, String)]) -> String {
        var s = "LUCH1;\(role)"
        for (k, v) in fields {
            let v = v.replacingOccurrences(of: ";", with: ",").replacingOccurrences(of: "\n", with: ",")
            if !v.isEmpty { s += ";\(k)=\(v)" }
        }
        return s
    }
}

/// A QR code: byte mode, error correction L, the smallest version that holds the text (1-12), mask 0, without the
/// quiet zone. The code is drawn sharp on a screen, and the fewer modules, the bigger each is for a camera across
/// the room.
public struct QrCode {
    private static let eccPerBlock = [-1, 7, 10, 15, 20, 26, 18, 20, 24, 30, 18, 20, 24]
    private static let blocks = [-1, 1, 1, 1, 1, 1, 2, 2, 2, 2, 4, 4, 4]
    /// L in the format bits.
    private static let level = 1

    public let size: Int
    private var modules: [[Bool]]
    private var function: [[Bool]]

    public func get(_ x: Int, _ y: Int) -> Bool { x >= 0 && y >= 0 && x < size && y < size && modules[y][x] }

    /// Rows of 0 and 1, for the page.
    public var rows: [String] { modules.map { String($0.map { $0 ? "1" : "0" }) } }

    /// nil when the text is too long for version 12.
    public init?(_ text: String) {
        let bytes = Array(text.utf8)
        for version in 1...12 {
            let capacity = Self.rawModules(version) / 8 - Self.eccPerBlock[version] * Self.blocks[version]
            let countBits = version < 10 ? 8 : 16
            guard 4 + countBits + bytes.count * 8 <= capacity * 8 else { continue }
            var bits: [Bool] = []
            func append(_ value: Int, _ count: Int) {
                for i in stride(from: count - 1, through: 0, by: -1) { bits.append((value >> i) & 1 != 0) }
            }
            append(0x4, 4)
            append(bytes.count, countBits)
            bytes.forEach { append(Int($0), 8) }
            bits += Array(repeating: false, count: min(4, capacity * 8 - bits.count))
            bits += Array(repeating: false, count: (8 - bits.count % 8) % 8)
            var data = [UInt8](repeating: 0, count: capacity)
            for (i, b) in bits.enumerated() where b { data[i >> 3] |= 1 << (7 - (i & 7)) }
            var pad: UInt8 = 0xEC
            for i in bits.count / 8..<capacity {
                data[i] = pad
                pad ^= 0xEC ^ 0x11
            }
            self.init(version: version, data: data)
            return
        }
        return nil
    }

    private init(version: Int, data: [UInt8]) {
        size = version * 4 + 17
        modules = Array(repeating: Array(repeating: false, count: size), count: size)
        function = modules
        drawFunctionPatterns(version)
        drawCodewords(Self.interleave(data, version))
        for y in 0..<size {
            for x in 0..<size where !function[y][x] && (x + y) % 2 == 0 { modules[y][x].toggle() }
        }
        drawFormatBits()
    }

    private static func rawModules(_ ver: Int) -> Int {
        var result = (16 * ver + 128) * ver + 64
        if ver >= 2 {
            let align = ver / 7 + 2
            result -= (25 * align - 10) * align - 55
            if ver >= 7 { result -= 36 }
        }
        return result
    }

    private mutating func set(_ x: Int, _ y: Int, _ dark: Bool) {
        modules[y][x] = dark
        function[y][x] = true
    }

    private mutating func drawFunctionPatterns(_ ver: Int) {
        for i in 0..<size {
            set(6, i, i % 2 == 0)
            set(i, 6, i % 2 == 0)
        }
        finder(3, 3)
        finder(size - 4, 3)
        finder(3, size - 4)
        let align = ver == 1 ? 0 : ver / 7 + 2
        var pos = [Int](repeating: 0, count: align)
        if align > 0 {
            let step = (ver * 4 + align * 2 + 1) / (align * 2 - 2) * 2
            pos[0] = 6
            var p = size - 7
            for i in stride(from: align - 1, through: 1, by: -1) {
                pos[i] = p
                p -= step
            }
        }
        for i in 0..<align {
            for j in 0..<align {
                if (i == 0 && j == 0) || (i == 0 && j == align - 1) || (i == align - 1 && j == 0) { continue }
                for dy in -2...2 {
                    for dx in -2...2 { set(pos[i] + dx, pos[j] + dy, max(abs(dx), abs(dy)) != 1) }
                }
            }
        }
        drawFormatBits()
        if ver >= 7 {
            var rem = ver
            for _ in 0..<12 { rem = (rem << 1) ^ ((rem >> 11) * 0x1F25) }
            let bits = ver << 12 | rem
            for i in 0..<18 {
                let bit = (bits >> i) & 1 != 0
                let a = size - 11 + i % 3, b = i / 3
                set(a, b, bit)
                set(b, a, bit)
            }
        }
    }

    private mutating func finder(_ cx: Int, _ cy: Int) {
        for dy in -4...4 {
            for dx in -4...4 {
                let dist = max(abs(dx), abs(dy))
                let x = cx + dx, y = cy + dy
                if x >= 0 && x < size && y >= 0 && y < size { set(x, y, dist != 2 && dist != 4) }
            }
        }
    }

    /// The error correction level and mask 0 in the format bits.
    private mutating func drawFormatBits() {
        let data = Self.level << 3
        var rem = data
        for _ in 0..<10 { rem = (rem << 1) ^ ((rem >> 9) * 0x537) }
        let bits = (data << 10 | rem) ^ 0x5412
        func bit(_ i: Int) -> Bool { (bits >> i) & 1 != 0 }
        for i in 0...5 { set(8, i, bit(i)) }
        set(8, 7, bit(6))
        set(8, 8, bit(7))
        set(7, 8, bit(8))
        for i in 9..<15 { set(14 - i, 8, bit(i)) }
        for i in 0..<8 { set(size - 1 - i, 8, bit(i)) }
        for i in 8..<15 { set(8, size - 15 + i, bit(i)) }
        set(8, size - 8, true)
    }

    private mutating func drawCodewords(_ data: [UInt8]) {
        var i = 0
        var right = size - 1
        while right >= 1 {
            if right == 6 { right = 5 }
            for vert in 0..<size {
                for j in 0..<2 {
                    let x = right - j
                    let upward = (right + 1) & 2 == 0
                    let y = upward ? size - 1 - vert : vert
                    if !function[y][x] && i < data.count * 8 {
                        modules[y][x] = (data[i >> 3] >> (7 - (i & 7))) & 1 != 0
                        i += 1
                    }
                }
            }
            right -= 2
        }
    }

    private static func interleave(_ data: [UInt8], _ ver: Int) -> [UInt8] {
        let blocks = Self.blocks[ver], ecc = eccPerBlock[ver]
        let raw = rawModules(ver) / 8
        let shortLen = raw / blocks, shortCount = blocks - raw % blocks
        let div = divisor(ecc)
        var all: [[UInt8]] = []
        var k = 0
        for i in 0..<blocks {
            let len = shortLen - ecc + (i < shortCount ? 0 : 1)
            let dat = Array(data[k..<k + len])
            k += len
            var block = dat + [UInt8](repeating: 0, count: shortLen + 1 - len)
            block.replaceSubrange(block.count - ecc..<block.count, with: remainder(dat, div))
            all.append(block)
        }
        var out: [UInt8] = []
        out.reserveCapacity(raw)
        for i in 0..<all[0].count {
            for j in 0..<blocks where i != shortLen - ecc || j >= shortCount { out.append(all[j][i]) }
        }
        return out
    }

    private static func divisor(_ degree: Int) -> [UInt8] {
        var result = [UInt8](repeating: 0, count: degree)
        result[degree - 1] = 1
        var root = 1
        for _ in 0..<degree {
            for j in 0..<degree {
                result[j] = UInt8(multiply(Int(result[j]), root))
                if j + 1 < degree { result[j] ^= result[j + 1] }
            }
            root = multiply(root, 0x02)
        }
        return result
    }

    private static func remainder(_ data: [UInt8], _ divisor: [UInt8]) -> [UInt8] {
        var result = [UInt8](repeating: 0, count: divisor.count)
        for b in data {
            let factor = Int(b ^ result[0])
            result.removeFirst()
            result.append(0)
            for i in 0..<result.count { result[i] ^= UInt8(multiply(Int(divisor[i]), factor)) }
        }
        return result
    }

    private static func multiply(_ x: Int, _ y: Int) -> Int {
        var z = 0
        for i in stride(from: 7, through: 0, by: -1) {
            z = (z << 1) ^ ((z >> 7) * 0x11D)
            z ^= ((y >> i) & 1) * x
        }
        return z
    }
}
