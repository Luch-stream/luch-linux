// Just enough of a zip reader for .pptx and .odp: the central directory, stored and deflated members. Foundation has
// no zip API, and running unzip once per slide would be slow on a long deck.
// SPDX-License-Identifier: GPL-3.0-or-later
import Compression
import Foundation

struct ZipArchive {
    private struct Member {
        let method: UInt16
        let compressed: Int
        let size: Int
        let offset: Int
    }

    private let data: Data
    private var members: [String: Member] = [:]

    /// A member's text is read up to this, so a crafted file cannot fill the memory (as luchd's notes.rs).
    static let maxMember = 32 << 20

    init?(url: URL) {
        guard let d = try? Data(contentsOf: url, options: .alwaysMapped) else { return nil }
        self.init(data: d)
    }

    init?(data: Data) {
        self.data = data
        // The end of central directory record: within the last 64 KiB (its comment) plus its own 22 bytes.
        let bytes = [UInt8](data.suffix(65_557))
        let base = data.count - bytes.count
        guard bytes.count >= 22,
              let eocd = stride(from: bytes.count - 22, through: 0, by: -1).first(where: {
                  bytes[$0] == 0x50 && bytes[$0 + 1] == 0x4b && bytes[$0 + 2] == 0x05 && bytes[$0 + 3] == 0x06
              }) else { return nil }
        let count = Int(u16(base + eocd + 10))
        var at = Int(u32(base + eocd + 16))
        for _ in 0..<count {
            guard at + 46 <= data.count, u32(at) == 0x0201_4b50 else { break }
            let nameLen = Int(u16(at + 28)), extraLen = Int(u16(at + 30)), commentLen = Int(u16(at + 32))
            guard at + 46 + nameLen <= data.count else { break }
            let name = String(decoding: data[(data.startIndex + at + 46)..<(data.startIndex + at + 46 + nameLen)],
                              as: UTF8.self)
            members[name] = Member(method: u16(at + 10), compressed: Int(u32(at + 20)), size: Int(u32(at + 24)),
                                   offset: Int(u32(at + 42)))
            at += 46 + nameLen + extraLen + commentLen
        }
    }

    private func u16(_ i: Int) -> UInt16 {
        let s = data.startIndex + i
        return UInt16(data[s]) | UInt16(data[s + 1]) << 8
    }

    private func u32(_ i: Int) -> UInt32 {
        UInt32(u16(i)) | UInt32(u16(i + 2)) << 16
    }

    var names: [String] { Array(members.keys) }

    /// A member's bytes; nil when it is missing, too big or packed in a way this does not read.
    func read(_ name: String) -> Data? {
        guard let m = members[name], m.size <= ZipArchive.maxMember, m.offset + 30 <= data.count,
              u32(m.offset) == 0x0403_4b50 else { return nil }
        let start = m.offset + 30 + Int(u16(m.offset + 26)) + Int(u16(m.offset + 28))
        guard start + m.compressed <= data.count else { return nil }
        let raw = data[(data.startIndex + start)..<(data.startIndex + start + m.compressed)]
        switch m.method {
        case 0:
            return Data(raw)
        case 8:
            if m.size == 0 { return Data() }
            var out = Data(count: m.size)
            let n = out.withUnsafeMutableBytes { dst in
                raw.withUnsafeBytes { src in
                    // COMPRESSION_ZLIB is raw DEFLATE (RFC 1951), which is what zip stores.
                    compression_decode_buffer(dst.bindMemory(to: UInt8.self).baseAddress!, m.size,
                                              src.bindMemory(to: UInt8.self).baseAddress!, raw.count,
                                              nil, COMPRESSION_ZLIB)
                }
            }
            return n == m.size ? out : nil
        default:
            return nil
        }
    }

    func text(_ name: String) -> String? {
        read(name).map { String(decoding: $0, as: UTF8.self) }
    }
}
