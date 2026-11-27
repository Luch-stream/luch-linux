// One browser as a Luch receiver on loopback, for the Engine: a control port that speaks JSON lines (the Engine's
// hello is answered with a welcome made from the page's hello, everything else goes to and from the page as
// websocket text), and two UDP ports whose shards are put back together into whole frames, sent to the page as
// websocket binary messages with the 24-byte Luch header first (what web.html reads). luchd does the same by giving
// the session a websocket instead of UDP (run_web).
// SPDX-License-Identifier: GPL-3.0-or-later
import Darwin
import Foundation
import LuchNet
import LuchProto
import LuchTV

final class WebBridge {
    let ws: WebSocket
    let receiver: Receiver
    let welcome: [String: Any]
    private let controlFD: Int32
    private let videoFD: Int32
    private let audioFD: Int32
    private let videoPort: UInt16
    private let audioPort: UInt16
    private let lock = NSLock()
    private var engineFD: Int32 = -1
    private var ended = false

    init(ws: WebSocket, hello: [String: Any], peer: String) throws {
        self.ws = ws
        let (c, cp) = try Self.listen(SOCK_STREAM)
        let (v, vp) = try Self.listen(SOCK_DGRAM)
        let (a, ap) = try Self.listen(SOCK_DGRAM)
        (controlFD, videoFD, audioFD, videoPort, audioPort) = (c, v, a, vp, ap)
        let name = hello["name"] as? String ?? "Web"
        let model = hello["model"] as? String ?? ""
        receiver = Receiver(id: "web-\(UUID().uuidString.lowercased())", name: name, model: "\(model) at \(peer)",
                            address: "127.0.0.1", interfaceName: "lo0", interfaceIndex: if_nametoindex("lo0"),
                            port: cp, service: "web")
        welcome = Self.welcome(hello)
    }

    /// What the Engine is told the browser can do: H.264 (WebCodecs' HEVC is not everywhere) and Opus, at its screen.
    static func welcome(_ hello: [String: Any]) -> [String: Any] {
        let display = hello["display"] as? [String: Any] ?? [:]
        let w = (display["width"] as? NSNumber)?.intValue ?? 1920
        let h = (display["height"] as? NSNumber)?.intValue ?? 1080
        let big = max(w, h) >= 3000
        return ["t": "welcome", "v": Int(Proto.version), "name": hello["name"] as? String ?? "Web",
                "model": hello["model"] as? String ?? "", "modes": ["mirror", "audio"], "decoders": ["video/avc"],
                "bench": ["prefer": "video/avc"],
                "hw": ["audio": ["audio/opus"],
                       "display": ["current": "\(w)x\(h)@60", "refresh": 60],
                       "video": ["video/avc": ["max": big ? "3840x2160" : "1920x1080", "1080p60": true, "2160p60": big,
                                               "low_latency": true]]]]
    }

    /// A socket on 127.0.0.1 and a free port.
    static func listen(_ type: Int32) throws -> (Int32, UInt16) {
        let fd = socket(AF_INET, type, 0)
        guard fd >= 0 else { throw TVError("socket: \(errnoString())") }
        var a = sockaddr_in()
        a.sin_len = UInt8(MemoryLayout<sockaddr_in>.size)
        a.sin_family = sa_family_t(AF_INET)
        inet_pton(AF_INET, "127.0.0.1", &a.sin_addr)
        let bound = withUnsafePointer(to: &a) {
            $0.withMemoryRebound(to: sockaddr.self, capacity: 1) { bind(fd, $0, socklen_t(MemoryLayout<sockaddr_in>.size)) }
        }
        guard bound == 0, type != SOCK_STREAM || Darwin.listen(fd, 4) == 0 else {
            close(fd)
            throw TVError("bind: \(errnoString())")
        }
        if type == SOCK_DGRAM {
            var size: Int32 = 4 << 20
            setsockopt(fd, SOL_SOCKET, SO_RCVBUF, &size, socklen_t(MemoryLayout<Int32>.size))
        }
        var len = socklen_t(MemoryLayout<sockaddr_in>.size)
        _ = withUnsafeMutablePointer(to: &a) { $0.withMemoryRebound(to: sockaddr.self, capacity: 1) { getsockname(fd, $0, &len) } }
        return (fd, UInt16(bigEndian: a.sin_port))
    }

    /// Runs until the browser or the bridge goes.
    func run() {
        Thread.detachNewThread { [self] in acceptEngines() }
        Thread.detachNewThread { [self] in frames(videoFD) }
        Thread.detachNewThread { [self] in frames(audioFD) }
        Thread.detachNewThread { [self] in pingWhileIdle() }
        // The page answers pings within a second; silence for this long means it is gone (a TV switched off).
        while let m = ws.receive(timeout: 30) {
            guard case .text(let t) = m, var v = try? JSONSerialization.jsonObject(with: Data(t.utf8)) as? [String: Any] else {
                continue
            }
            switch v["t"] as? String {
            case "bye":
                end()
                return
            case "ready":
                // The page has no ports of its own: the media comes to ours and goes on over its websocket.
                v["video_port"] = Int(videoPort)
                v["audio_port"] = Int(audioPort)
            default: break
            }
            toEngine(v)
        }
        end()
    }

    /// Ends it: the Engine is told the receiver left, the page that the stream ended.
    func end() {
        lock.lock()
        let was = ended
        ended = true
        let e = engineFD
        engineFD = -1
        lock.unlock()
        guard !was else { return }
        if e >= 0 {
            Self.line(e, ["t": "bye"])
            close(e)
        }
        for fd in [controlFD, videoFD, audioFD] {
            shutdown(fd, SHUT_RDWR)
            close(fd)
        }
        ws.send(json: ["t": "bye"])
        ws.close()
    }

    private func toEngine(_ v: [String: Any]) {
        lock.lock()
        let e = engineFD
        lock.unlock()
        if e >= 0 { Self.line(e, v) }
    }

    @discardableResult
    static func line(_ fd: Int32, _ v: [String: Any]) -> Bool {
        guard var d = try? JSONSerialization.data(withJSONObject: v) else { return false }
        d.append(0x0a)
        return d.withUnsafeBytes { p in
            var off = 0
            while off < p.count {
                let n = send(fd, p.baseAddress!.advanced(by: off), p.count - off, 0)
                if n <= 0 { return false }
                off += n
            }
            return true
        }
    }

    /// The Engine's control connections, one at a time (a new session takes over).
    private func acceptEngines() {
        while true {
            let c = accept(controlFD, nil, nil)
            if c < 0 { return }
            var one: Int32 = 1
            setsockopt(c, SOL_SOCKET, SO_NOSIGPIPE, &one, socklen_t(MemoryLayout<Int32>.size))
            lock.lock()
            let old = engineFD
            engineFD = ended ? -1 : c
            lock.unlock()
            if old >= 0 { close(old) }
            Thread.detachNewThread { [self] in engineLines(c) }
        }
    }

    /// What the Engine says: its hello is answered here, everything else goes to the page.
    private func engineLines(_ c: Int32) {
        var pending = Data()
        var buf = [UInt8](repeating: 0, count: 65536)
        while true {
            let n = recv(c, &buf, buf.count, 0)
            if n <= 0 { break }
            pending.append(contentsOf: buf[0..<n])
            while let nl = pending.firstIndex(of: 0x0a) {
                let line = pending[pending.startIndex..<nl]
                pending.removeSubrange(pending.startIndex...nl)
                guard let v = try? JSONSerialization.jsonObject(with: Data(line)) as? [String: Any] else { continue }
                if v["t"] as? String == "hello" {
                    Self.line(c, welcome)
                } else {
                    ws.send(.text(String(decoding: line, as: UTF8.self)))
                }
            }
        }
        lock.lock()
        if engineFD == c { engineFD = -1 }
        lock.unlock()
    }

    /// Without an Engine nothing asks the page whether it is there: we do, as luchd does.
    private func pingWhileIdle() {
        var n = 0
        while true {
            Thread.sleep(forTimeInterval: 5)
            lock.lock()
            let (done, idle) = (ended, engineFD < 0)
            lock.unlock()
            if done { return }
            if idle {
                n += 1
                ws.send(json: ["t": "ping", "n": n])
            }
        }
    }

    /// Shards in, whole frames out: loopback keeps them in order, so a frame is its shards one after another.
    private func frames(_ fd: Int32) {
        var buf = [UInt8](repeating: 0, count: 65536)
        var head: [UInt8] = []
        var body = Data()
        var seq: UInt32 = 0
        var next: UInt16 = 0
        while true {
            let n = recv(fd, &buf, buf.count, 0)
            if n <= 0 { return }
            guard n > Proto.headerSize else { continue }
            let p = Array(buf[0..<n])
            let be = { (at: Int, n: Int) in p[at..<at + n].reduce(UInt32(0)) { $0 << 8 | UInt32($1) } }
            let shard = UInt16(be(2, 2)), shards = UInt16(be(4, 2)), s = be(8, 4), len = Int(be(16, 4))
            if shard == 0 {
                head = Array(p[0..<Proto.headerSize])
                body = Data()
                seq = s
                next = 0
            }
            // A shard lost or out of turn: the frame is dropped, the page asks for a key frame if it needs one.
            guard s == seq, shard == next else {
                next = UInt16.max
                continue
            }
            body.append(contentsOf: p[Proto.headerSize...])
            next += 1
            if next == shards, body.count >= len {
                ws.send(.binary(Data(head) + body.prefix(len)))
                next = UInt16.max
            }
        }
    }
}
