// A small RFC 6455 websocket over a blocking socket, for the presenter page on this Mac: text frames, ping and close.
// The page sends short JSON lines; anything over a megabyte is refused.
// SPDX-License-Identifier: GPL-3.0-or-later
import CryptoKit
import Darwin
import Foundation

final class WebSocket {
    static let maxMessage = 1 << 20

    private let fd: Int32
    private var carry: Data
    private let writeLock = NSLock()
    private var closed = false

    /// `carry`: what was read past the request head.
    init(fd: Int32, carry: Data) {
        self.fd = fd
        self.carry = carry
    }

    /// The answer to `Sec-WebSocket-Key`.
    static func accept(key: String) -> String {
        let digest = Insecure.SHA1.hash(data: Data((key + "258EAFA5-E914-47DA-95CA-C5AB0DC85B11").utf8))
        return Data(digest).base64EncodedString()
    }

    static func handshake(key: String) -> Data {
        Data(("HTTP/1.1 101 Switching Protocols\r\nUpgrade: websocket\r\nConnection: Upgrade\r\n" +
              "Sec-WebSocket-Accept: \(accept(key: key))\r\n\r\n").utf8)
    }

    private func fill(_ n: Int) -> Bool {
        while carry.count < n {
            var chunk = [UInt8](repeating: 0, count: 16 * 1024)
            let got = recv(fd, &chunk, chunk.count, 0)
            if got <= 0 {
                if got < 0 && errno == EINTR { continue }
                return false
            }
            carry.append(contentsOf: chunk[0..<got])
        }
        return true
    }

    private func take(_ n: Int) -> Data {
        let d = Data(carry.prefix(n))
        carry.removeFirst(n)
        return d
    }

    /// The next text message; nil when the connection ends or breaks the rules.
    func receive() -> String? {
        var message = Data()
        while true {
            guard fill(2) else { return nil }
            let head = [UInt8](take(2))
            let fin = head[0] & 0x80 != 0, op = head[0] & 0x0f
            var len = Int(head[1] & 0x7f)
            // A client always masks its frames.
            guard head[1] & 0x80 != 0 else { return nil }
            if len == 126 {
                guard fill(2) else { return nil }
                len = [UInt8](take(2)).reduce(0) { $0 << 8 | Int($1) }
            } else if len == 127 {
                guard fill(8) else { return nil }
                let big = [UInt8](take(8)).reduce(UInt64(0)) { $0 << 8 | UInt64($1) }
                guard big <= UInt64(WebSocket.maxMessage) else { return nil }
                len = Int(big)
            }
            guard len + message.count <= WebSocket.maxMessage, fill(4 + len) else { return nil }
            let mask = [UInt8](take(4))
            var payload = [UInt8](take(len))
            for i in payload.indices { payload[i] ^= mask[i % 4] }
            switch op {
            case 0x0, 0x1, 0x2:
                message.append(contentsOf: payload)
                if fin { return String(decoding: message, as: UTF8.self) }
            case 0x8:
                close()
                return nil
            case 0x9:
                write(op: 0xA, Data(payload))
            default:
                break
            }
        }
    }

    @discardableResult
    func send(_ text: String) -> Bool {
        write(op: 0x1, Data(text.utf8))
    }

    @discardableResult
    private func write(op: UInt8, _ payload: Data) -> Bool {
        var frame = Data([0x80 | op])
        if payload.count < 126 {
            frame.append(UInt8(payload.count))
        } else if payload.count <= 0xffff {
            frame.append(126)
            frame.append(contentsOf: [UInt8(payload.count >> 8), UInt8(payload.count & 0xff)])
        } else {
            frame.append(127)
            frame.append(contentsOf: (0..<8).reversed().map { UInt8(truncatingIfNeeded: payload.count >> ($0 * 8)) })
        }
        frame.append(payload)
        writeLock.lock()
        defer { writeLock.unlock() }
        guard !closed else { return false }
        return HTTP.write(fd, frame)
    }

    /// Sends a close frame and ends the connection; the reader then sees the end.
    func close() {
        write(op: 0x8, Data())
        writeLock.lock()
        closed = true
        writeLock.unlock()
        shutdown(fd, SHUT_RDWR)
    }
}

/// The few HTTP pieces the presenter view needs.
enum HTTP {
    struct Request {
        var method = ""
        var target = ""
        var headers: [String: String] = [:]
    }

    /// One request head; nil when the connection ends first or the head is too long.
    static func read(_ fd: Int32, carry: inout Data) -> Request? {
        let end = Data("\r\n\r\n".utf8)
        while carry.range(of: end) == nil {
            if carry.count > 16 * 1024 { return nil }
            var chunk = [UInt8](repeating: 0, count: 4096)
            let n = recv(fd, &chunk, chunk.count, 0)
            if n <= 0 { return nil }
            carry.append(contentsOf: chunk[0..<n])
        }
        let r = carry.range(of: end)!
        let head = String(decoding: carry[carry.startIndex..<r.lowerBound], as: UTF8.self)
        carry.removeSubrange(carry.startIndex..<r.upperBound)
        var lines = head.components(separatedBy: "\r\n")
        let first = lines.removeFirst().split(separator: " ")
        var q = Request()
        q.method = first.first.map(String.init) ?? ""
        q.target = first.count > 1 ? String(first[1]) : "/"
        for l in lines {
            guard let colon = l.firstIndex(of: ":") else { continue }
            q.headers[l[..<colon].trimmingCharacters(in: .whitespaces).lowercased()] =
                l[l.index(after: colon)...].trimmingCharacters(in: .whitespaces)
        }
        return q
    }

    @discardableResult
    static func write(_ fd: Int32, _ data: Data) -> Bool {
        data.withUnsafeBytes { (p: UnsafeRawBufferPointer) -> Bool in
            var off = 0
            while off < p.count {
                let n = Darwin.send(fd, p.baseAddress!.advanced(by: off), p.count - off, 0)
                if n <= 0 { if n < 0 && errno == EINTR { continue }; return false }
                off += n
            }
            return true
        }
    }

    static func reply(_ fd: Int32, _ status: String, _ type: String, _ body: Data) {
        let head = "HTTP/1.1 \(status)\r\nContent-Type: \(type)\r\nContent-Length: \(body.count)\r\n" +
            "Cache-Control: no-store\r\nX-Content-Type-Options: nosniff\r\nReferrer-Policy: no-referrer\r\n" +
            "Connection: close\r\n\r\n"
        _ = write(fd, Data(head.utf8)) && write(fd, body)
    }
}
