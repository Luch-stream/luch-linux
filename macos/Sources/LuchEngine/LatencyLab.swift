// The latency lab's sender side (PROTOCOL.md 4j, luch-linux docs/latency-lab.md): a page for this Mac's screen that
// counts its refreshes in Gray code beside a QR code of how the stream is set up, served on the loopback interface
// only. A browser page, as on Linux: Safari paints on vsync and says when (requestAnimationFrame) and goes full
// screen on a click; the page only draws, the layout and the QR code come from here.
// SPDX-License-Identifier: GPL-3.0-or-later
import Darwin
import Foundation
import LuchProto

public final class LatencyLab {
    /// The receiver the lab is for.
    public let id: String
    public let url: String
    private let stopped = Stopped()

    /// The session's numbers in luchd's shape (`sender`: rtt_ms, host_ms, mbps, encoder, res; `est_ms`), and how its
    /// screen is shown; read for every QR code the page asks for.
    public typealias Now = () -> (stats: [String: Any], mode: String)

    /// Serves the page for receiver `id` on a free port of 127.0.0.1.
    public init(id: String, now: @escaping Now) throws {
        let s = socket(AF_INET, SOCK_STREAM, 0)
        guard s >= 0 else { throw LabError.socket(errnoString()) }
        var one: Int32 = 1
        setsockopt(s, SOL_SOCKET, SO_REUSEADDR, &one, socklen_t(MemoryLayout<Int32>.size))
        var a = sockaddr_in()
        a.sin_len = UInt8(MemoryLayout<sockaddr_in>.size)
        a.sin_family = sa_family_t(AF_INET)
        a.sin_addr.s_addr = inet_addr("127.0.0.1")
        let size = socklen_t(MemoryLayout<sockaddr_in>.size)
        let bound = withUnsafePointer(to: &a) { $0.withMemoryRebound(to: sockaddr.self, capacity: 1) { bind(s, $0, size) } }
        var len = size
        withUnsafeMutablePointer(to: &a) { $0.withMemoryRebound(to: sockaddr.self, capacity: 1) { _ = getsockname(s, $0, &len) } }
        guard bound == 0, listen(s, 8) == 0 else {
            Darwin.close(s)
            throw LabError.socket(errnoString())
        }
        self.id = id
        url = "http://127.0.0.1:\(UInt16(bigEndian: a.sin_port))/"
        // The listening socket is closed by this thread alone, once told to stop: no accept on a reused descriptor.
        let stopped = self.stopped
        Thread.detachNewThread {
            defer { Darwin.close(s) }
            var pfd = pollfd(fd: s, events: Int16(POLLIN), revents: 0)
            while !stopped.value {
                guard poll(&pfd, 1, 200) > 0 else { continue }
                let c = accept(s, nil, nil)
                if c >= 0 { Thread.detachNewThread { Self.answer(c, now: now) } }
            }
        }
    }

    deinit { close() }

    /// Stops serving within 0.2 s; the page keeps counting with what it has.
    public func close() { stopped.set() }

    private static func answer(_ c: Int32, now: Now) {
        defer { Darwin.close(c) }
        var tv = timeval(tv_sec: 5, tv_usec: 0)
        setsockopt(c, SOL_SOCKET, SO_RCVTIMEO, &tv, socklen_t(MemoryLayout<timeval>.size))
        var head = [UInt8]()
        var byte: UInt8 = 0
        while !head.ends(with: [13, 10, 13, 10]) {
            guard head.count < 8192, recv(c, &byte, 1, 0) == 1 else { return }
            head.append(byte)
        }
        let target = String(decoding: head, as: UTF8.self).split(separator: " ").dropFirst().first.map(String.init) ?? "/"
        let parts = target.split(separator: "?", maxSplits: 1).map(String.init)
        let (status, kind, body): (String, String, String)
        switch parts.first ?? "/" {
        case "/":
            (status, kind, body) = ("200 OK", "text/html; charset=utf-8", LatencyPage.html)
        case "/meta":
            let q = params(parts.count > 1 ? parts[1] : "")
            let n = now()
            let meta = senderMeta(stats: n.stats, mode: n.mode, hz: q["hz"].flatMap(Double.init),
                                  missed: q["miss"].flatMap(Int.init))
            let data = (try? JSONSerialization.data(withJSONObject: pageData(meta))) ?? Data()
            (status, kind, body) = ("200 OK", "application/json", String(decoding: data, as: UTF8.self))
        default:
            (status, kind, body) = ("404 Not Found", "text/plain", "not here\n")
        }
        let bytes = Array(body.utf8)
        let response = Array(("HTTP/1.1 \(status)\r\nContent-Type: \(kind)\r\nContent-Length: \(bytes.count)\r\n" +
            "Cache-Control: no-store\r\nConnection: close\r\n\r\n").utf8) + bytes
        var sent = 0
        while sent < response.count {
            let n = response[sent...].withUnsafeBytes { Darwin.send(c, $0.baseAddress, $0.count, 0) }
            if n <= 0 { return }
            sent += n
        }
    }

    private static func params(_ query: String) -> [String: String] {
        var out: [String: String] = [:]
        for kv in query.split(separator: "&") {
            let p = kv.split(separator: "=", maxSplits: 1).map(String.init)
            if p.count == 2 { out[p[0]] = p[1] }
        }
        return out
    }

    /// What the sender's QR code says. Short keys keep the code small, and so its modules big for the camera; the
    /// analyser's report spells them out.
    public static func senderMeta(stats: [String: Any], mode: String, hz: Double?, missed: Int?) -> String {
        let s = stats["sender"] as? [String: Any] ?? [:]
        func num(_ v: Any?) -> String { (v as? NSNumber).map { String(format: "%.1f", $0.doubleValue) } ?? "" }
        let encoder = s["encoder"] as? String ?? ""
        let codec = encoder.hasPrefix("hevc") ? "hevc" : encoder.hasPrefix("h264") ? "h264" : ""
        return LatencyPattern.text(role: "S", [
            ("v", Proto.app), ("m", mode), ("c", codec), ("b", num(s["mbps"])), ("r", s["res"] as? String ?? ""),
            ("p", num(s["rtt_ms"])), ("h", num(s["host_ms"])), ("e", num(stats["est_ms"])),
            ("hz", hz.map { String(format: "%.2f", $0) } ?? ""), ("x", missed.map(String.init) ?? ""),
        ])
    }

    /// Everything the page draws from: the layout, the QR code's modules and its text.
    public static func pageData(_ text: String) -> [String: Any] {
        ["layout": LatencyPattern.layout, "qr": QrCode(text)?.rows ?? [], "text": text]
    }
}

private final class Stopped {
    private let lock = NSLock()
    private var stopped = false
    var value: Bool { lock.lock(); defer { lock.unlock() }; return stopped }
    func set() { lock.lock(); stopped = true; lock.unlock() }
}

public enum LabError: Error, LocalizedError {
    case socket(String)

    public var errorDescription: String? {
        switch self {
        case let .socket(why): return "latency lab: \(why)"
        }
    }
}

private extension Array where Element == UInt8 {
    func ends(with tail: [UInt8]) -> Bool { count >= tail.count && Array(suffix(tail.count)) == tail }
}
