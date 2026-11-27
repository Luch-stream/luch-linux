// A receiver on loopback that speaks just hello/welcome (and checks a token), as luchd's find tests use: or `junk`
// for something else on the port, or `silent` for one that accepts and says nothing.
// SPDX-License-Identifier: GPL-3.0-or-later
import Darwin
import Foundation

final class LoopbackReceiver {
    enum Kind { case receiver, junk, silent }
    let port: UInt16
    private let fd: Int32
    private let lock = NSLock()
    private var heard: [[String: Any]] = []

    /// Everything the receiver was told after its welcome.
    var requests: [[String: Any]] {
        lock.lock()
        defer { lock.unlock() }
        return heard
    }

    init(id: String = "", name: String = "", kind: Kind = .receiver, modes: [String] = []) {
        let fd = socket(AF_INET, SOCK_STREAM, 0)
        self.fd = fd
        var on: Int32 = 1
        setsockopt(fd, SOL_SOCKET, SO_REUSEADDR, &on, socklen_t(MemoryLayout<Int32>.size))
        var a = sockaddr_in()
        a.sin_len = UInt8(MemoryLayout<sockaddr_in>.size)
        a.sin_family = sa_family_t(AF_INET)
        a.sin_addr.s_addr = inet_addr("127.0.0.1")
        _ = withUnsafePointer(to: &a) { $0.withMemoryRebound(to: sockaddr.self, capacity: 1) { bind(fd, $0, socklen_t(MemoryLayout<sockaddr_in>.size)) } }
        listen(fd, 16)
        var len = socklen_t(MemoryLayout<sockaddr_in>.size)
        _ = withUnsafeMutablePointer(to: &a) { $0.withMemoryRebound(to: sockaddr.self, capacity: 1) { getsockname(fd, $0, &len) } }
        port = UInt16(bigEndian: a.sin_port)
        guard kind != .silent else { return }
        let listener = fd
        Thread.detachNewThread { [weak self] in
            while true {
                let c = accept(listener, nil, nil)
                guard c >= 0 else { return }
                Thread.detachNewThread { self?.serve(c, id: id, name: name, kind: kind, modes: modes) }
            }
        }
    }

    deinit { close(fd) }

    private func serve(_ c: Int32, id: String, name: String, kind: Kind, modes: [String]) {
        defer { close(c) }
        func say(_ obj: [String: Any]) {
            var d = try! JSONSerialization.data(withJSONObject: obj)
            d.append(0x0a)
            _ = d.withUnsafeBytes { send(c, $0.baseAddress, $0.count, 0) }
        }
        if kind == .junk {
            _ = "SSH-2.0-OpenSSH_9.6\r\n".withCString { send(c, $0, strlen($0), 0) }
            return
        }
        var pending = Data()
        var buf = [UInt8](repeating: 0, count: 4096)
        var welcomed = false
        while true {
            let n = recv(c, &buf, buf.count, 0)
            guard n > 0 else { return }
            pending.append(contentsOf: buf[0..<n])
            while let nl = pending.firstIndex(of: 0x0a) {
                let line = pending[pending.startIndex..<nl]
                pending.removeSubrange(pending.startIndex...nl)
                guard let m = (try? JSONSerialization.jsonObject(with: Data(line))) as? [String: Any] else { continue }
                if !welcomed, m["t"] as? String == "hello" {
                    welcomed = true
                    var w: [String: Any] = ["t": "welcome", "v": 0, "id": id, "name": name, "model": "Test TV",
                                            "modes": modes, "sdk": 22,
                                            "limits": ["audio_latency_ms": 120, "limits": ["audio_lag", "x_new"]]]
                    if let p = m["probe"] { w["probe"] = p }
                    if let t = m["token"] as? String { w["token"] = t == "0badf00d" ? "ok" : "bad" }
                    say(w)
                    continue
                }
                lock.lock()
                heard.append(m)
                lock.unlock()
                if m["t"] as? String == "ble_wake" { say(["t": "ble_wake", "ok": true]) }
                if m["t"] as? String == "bye" { return }
            }
        }
    }
}
