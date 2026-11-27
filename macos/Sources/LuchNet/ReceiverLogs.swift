// A receiver's own log, handed to a sender that asks for a problem report (PROTOCOL.md, after section 6):
//
//   sender   → {"t":"logs","max_kb":1024}
//   receiver → {"t":"logs","part":i,"parts":n,"gz":"<base64 of a slice of the gzip of the log text>"}  n lines
//   or         {"t":"error","for":"logs","code":"not_allowed"}
//
// Only a receiver whose welcome lists "logs" in `modes` is asked, and only over a connection it let in (paired over
// TLS): it never shows anything on the TV for it. The slices are joined in order and gunzipped here.
// SPDX-License-Identifier: GPL-3.0-or-later
import Foundation
import LuchProto
import zlib

public enum ReceiverLogs {
    public static let mode = "logs"
    /// How long a receiver may take to start answering, and between two slices.
    public static let timeout: TimeInterval = 10
    /// More than this unpacked is not a log: the rest is refused.
    static let maxText = 16 << 20

    public struct Failure: Error, LocalizedError {
        public let errorDescription: String?
        init(_ why: String) { errorDescription = why }
    }

    public static func request(maxKB: Int) -> [String: Any] { ["t": mode, "max_kb": maxKB] }

    /// The slices as they come, keyed by `part`; whole once `parts` distinct ones are in.
    public struct Assembler {
        private var slices: [Int: Data] = [:]
        private var parts: Int?

        public init() {}

        /// Takes one `logs` line; throws on a line that does not fit the others.
        public mutating func take(_ msg: [String: Any]) throws {
            guard let part = (msg["part"] as? NSNumber)?.intValue, let n = (msg["parts"] as? NSNumber)?.intValue,
                  n > 0, n <= 4096, let gz = msg["gz"] as? String, let data = Data(base64Encoded: gz) else {
                throw Failure("a log slice that cannot be read")
            }
            if let parts, parts != n { throw Failure("log slices of two different logs") }
            // Counted from 0 or from 1: both read the same, the order is what matters.
            guard part >= 0, part <= n else { throw Failure("log slice \(part) of \(n)") }
            parts = n
            slices[part] = data
        }

        public var isComplete: Bool {
            guard let parts, slices.count == parts, let lo = slices.keys.min(), let hi = slices.keys.max() else { return false }
            return hi - lo == parts - 1
        }

        /// The gzip the slices make up, in order.
        public var joined: Data { slices.keys.sorted().reduce(into: Data()) { $0.append(slices[$1]!) } }
    }

    /// Asks over an open channel and waits for the whole log; throws with the reason in words.
    public static func fetch(_ ctl: ControlChannel, maxKB: Int = 1024, timeout: TimeInterval = timeout) throws -> String {
        ctl.send(request(maxKB: maxKB))
        var a = Assembler()
        while !a.isComplete {
            guard let m = ctl.waitForAny([mode, "error"], timeout: timeout) else {
                throw Failure(ctl.isClosed ? "closed the connection before its log was in" : "did not send its log in time")
            }
            if m["t"] as? String == "error" {
                guard m["for"] as? String == mode else { continue } // about something else
                switch m["code"] as? String {
                case "not_allowed": throw Failure("would not hand over its log to this Mac (not let in)")
                case "unknown": throw Failure("cannot hand over its log: update Luch on it")
                default: throw Failure("refused its log: \(m["detail"] as? String ?? m["code"] as? String ?? "?")")
                }
            }
            try a.take(m)
        }
        guard let text = gunzip(a.joined, limit: maxText) else { throw Failure("sent a log that does not unpack") }
        return String(decoding: text, as: UTF8.self)
    }

    /// Unpacks gzip (members one after another too); nil when it is not gzip, is cut short or unpacks past `limit`.
    public static func gunzip(_ data: Data, limit: Int) -> Data? {
        guard data.count >= 18, data[data.startIndex] == 0x1f, data[data.startIndex + 1] == 0x8b else { return nil }
        var s = z_stream()
        // 16 + 15: a gzip header and trailer around the deflate stream, its CRC checked by zlib.
        guard inflateInit2_(&s, 16 + MAX_WBITS, ZLIB_VERSION, Int32(MemoryLayout<z_stream>.size)) == Z_OK else { return nil }
        defer { inflateEnd(&s) }
        var input = [UInt8](data)
        var out = Data()
        var chunk = [UInt8](repeating: 0, count: 64 * 1024)
        return input.withUnsafeMutableBufferPointer { inBuf -> Data? in
            s.next_in = inBuf.baseAddress
            s.avail_in = uInt(inBuf.count)
            while true {
                let rc = chunk.withUnsafeMutableBufferPointer { outBuf -> Int32 in
                    s.next_out = outBuf.baseAddress
                    s.avail_out = uInt(outBuf.count)
                    return inflate(&s, Z_NO_FLUSH)
                }
                out.append(contentsOf: chunk[0..<(chunk.count - Int(s.avail_out))])
                if out.count > limit { return nil }
                switch rc {
                case Z_STREAM_END:
                    if s.avail_in == 0 { return out }
                    guard inflateReset(&s) == Z_OK else { return nil } // the next member
                case Z_OK:
                    continue
                case Z_BUF_ERROR where s.avail_in > 0:
                    continue
                default:
                    return nil // corrupt, or cut short
                }
            }
        }
    }
}
