// The server side of a WebSocket (RFC 6455) over a byte stream: the handshake's accept key, and frames: text and
// binary out unmasked, in masked and possibly fragmented, pings answered, a close returned.
// SPDX-License-Identifier: GPL-3.0-or-later
import CryptoKit
import Foundation
import LuchTV

public final class WebSocket {
    public enum Message: Equatable {
        case text(String)
        case binary(Data)
    }

    let stream: ByteStream
    private let writeLock = NSLock()
    private var closed = false
    /// The most one message may be: control and the page's answers are small.
    static let maxMessage = 1 << 20

    public init(_ stream: ByteStream) {
        self.stream = stream
    }

    /// Sec-WebSocket-Accept for a Sec-WebSocket-Key.
    public static func accept(_ key: String) -> String {
        Data(Insecure.SHA1.hash(data: Data((key + "258EAFA5-E914-47DA-95CA-C5AB0DC85B11").utf8))).base64EncodedString()
    }

    /// The 101 answer to an upgrade request with `key`.
    public static func handshake(_ key: String) -> Data {
        Data(("HTTP/1.1 101 Switching Protocols\r\nUpgrade: websocket\r\nConnection: Upgrade\r\n"
              + "Sec-WebSocket-Accept: \(accept(key))\r\n\r\n").utf8)
    }

    /// One frame as the server sends it (FIN, no mask).
    public static func frame(_ opcode: UInt8, _ payload: Data) -> Data {
        var out = Data([0x80 | opcode])
        let n = payload.count
        if n < 126 {
            out.append(UInt8(n))
        } else if n <= 0xffff {
            out.append(126)
            out.append(contentsOf: [UInt8(n >> 8), UInt8(n & 0xff)])
        } else {
            out.append(127)
            out.append(contentsOf: (0..<8).reversed().map { UInt8(truncatingIfNeeded: UInt64(n) >> (UInt64($0) * 8)) })
        }
        return out + payload
    }

    @discardableResult
    public func send(_ m: Message) -> Bool {
        let f: Data
        switch m {
        case .text(let t): f = Self.frame(1, Data(t.utf8))
        case .binary(let b): f = Self.frame(2, b)
        }
        return write(f)
    }

    @discardableResult
    public func send(json v: [String: Any]) -> Bool {
        guard let d = try? JSONSerialization.data(withJSONObject: v) else { return false }
        return send(.text(String(decoding: d, as: UTF8.self)))
    }

    private func write(_ f: Data) -> Bool {
        writeLock.lock()
        defer { writeLock.unlock() }
        return !closed && stream.write(f)
    }

    /// Says goodbye and closes the stream.
    public func close() {
        _ = write(Self.frame(8, Data([0x03, 0xe8])))
        writeLock.lock()
        closed = true
        writeLock.unlock()
        stream.close()
    }

    /// The next whole message; nil at the end (a close, an error, garbage, `timeout` seconds of nothing).
    public func receive(timeout: Double = .infinity) -> Message? {
        var opcode: UInt8 = 0
        var payload = Data()
        while true {
            guard let h = stream.read(2, timeout: timeout) else { return nil }
            let fin = h[0] & 0x80 != 0
            let op = h[0] & 0x0f
            let masked = h[1] & 0x80 != 0
            var n = Int(h[1] & 0x7f)
            if n == 126 {
                guard let e = stream.read(2, timeout: timeout) else { return nil }
                n = Int(e[0]) << 8 | Int(e[1])
            } else if n == 127 {
                guard let e = stream.read(8, timeout: timeout) else { return nil }
                let v = e.reduce(UInt64(0)) { $0 << 8 | UInt64($1) }
                guard v <= UInt64(Self.maxMessage) else { return nil }
                n = Int(v)
            }
            guard n <= Self.maxMessage, payload.count + n <= Self.maxMessage else { return nil }
            // A client's frames are masked (RFC 6455 5.1).
            guard masked, let key = stream.read(4, timeout: timeout) else { return nil }
            var data = n > 0 ? (stream.read(n, timeout: timeout) ?? Data()) : Data()
            guard data.count == n else { return nil }
            let k = [UInt8](key)
            data.withUnsafeMutableBytes { (p: UnsafeMutableRawBufferPointer) in
                for i in 0..<n { p[i] ^= k[i & 3] }
            }
            switch op {
            case 8:
                close()
                return nil
            case 9:
                _ = write(Self.frame(10, data))
                continue
            case 10:
                continue
            case 0:
                payload.append(data)
            default:
                opcode = op
                payload = data
            }
            if fin {
                switch opcode {
                case 1: return .text(String(decoding: payload, as: UTF8.self))
                case 2: return .binary(payload)
                default: return nil
                }
            }
        }
    }
}
