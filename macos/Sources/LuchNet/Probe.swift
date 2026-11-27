// Asking an address whether a receiver is there (PROTOCOL.md 1b): a TCP connect bound to the LAN interface, a
// probe hello, the welcome, bye. A plain blocking socket with deadlines, light enough for a hundred at once.
// SPDX-License-Identifier: GPL-3.0-or-later
import Darwin
import Foundation
import LuchProto

public enum ProbeTimes {
    /// No host on the LAN takes longer than this to accept; a missing one is given up after it.
    public static let connect: TimeInterval = 0.4
    /// A receiver answers hello within 2 s (section 3).
    public static let welcome: TimeInterval = 2
}

/// A connected TCP socket to `ip:port` from interface `ifindex` (0: any), or nil after `timeout`.
public func tcpConnect(_ ip: UInt32, _ port: UInt16, ifindex: UInt32, timeout: TimeInterval) -> Int32? {
    let s = socket(AF_INET, SOCK_STREAM, 0)
    guard s >= 0 else { return nil }
    var on: Int32 = 1
    setsockopt(s, SOL_SOCKET, SO_NOSIGPIPE, &on, socklen_t(MemoryLayout<Int32>.size))
    if ifindex != 0 {
        var idx = ifindex
        setsockopt(s, IPPROTO_IP, IP_BOUND_IF, &idx, socklen_t(MemoryLayout<UInt32>.size))
    }
    _ = fcntl(s, F_SETFL, O_NONBLOCK)
    var addr = sockaddr_in()
    addr.sin_len = UInt8(MemoryLayout<sockaddr_in>.size)
    addr.sin_family = sa_family_t(AF_INET)
    addr.sin_port = port.bigEndian
    addr.sin_addr.s_addr = ip.bigEndian
    let rc = withUnsafePointer(to: &addr) {
        $0.withMemoryRebound(to: sockaddr.self, capacity: 1) { connect(s, $0, socklen_t(MemoryLayout<sockaddr_in>.size)) }
    }
    if rc != 0 {
        guard errno == EINPROGRESS else { close(s); return nil }
        var pfd = pollfd(fd: s, events: Int16(POLLOUT), revents: 0)
        var err: Int32 = 0
        var len = socklen_t(MemoryLayout<Int32>.size)
        guard poll(&pfd, 1, Int32(timeout * 1000)) == 1, getsockopt(s, SOL_SOCKET, SO_ERROR, &err, &len) == 0, err == 0 else {
            close(s)
            return nil
        }
    }
    return s
}

/// Whether something accepts on `ip:port` now (a receiver found other ways than mDNS is dropped when not).
public func answers(_ address: String, _ port: UInt16) -> Bool {
    guard let ip = parseIPv4(address), let s = tcpConnect(ip, port, ifindex: interfaceFor(ip)?.index ?? 0,
                                                          timeout: 1) else { return false }
    close(s)
    return true
}

/// Who asks in a probe.
public struct ProbeIdentity {
    public var id: String
    public var name: String
    public init(id: String, name: String) {
        self.id = id
        self.name = name
    }
}

/// The receiver's welcome at `ip:port`, asked with a probe hello (and the pairing token from its QR code or sound,
/// when there is one), or nil when nothing there speaks Luch.
public func probe(_ ip: UInt32, _ port: UInt16, ifindex: UInt32, me: ProbeIdentity, token: String? = nil,
                  connectTimeout: TimeInterval = ProbeTimes.connect) -> [String: Any]? {
    guard let s = tcpConnect(ip, port, ifindex: ifindex, timeout: connectTimeout) else { return nil }
    defer { close(s) }
    var hello: [String: Any] = ["t": "hello", "v": Int(Proto.version), "id": me.id, "name": me.name,
                                "app": Proto.app, "want": [String](), "probe": true]
    if let token { hello["token"] = token }
    guard writeLine(s, hello), let w = readLine(s, timeout: ProbeTimes.welcome) else { return nil }
    _ = writeLine(s, ["t": "bye"])
    return w
}

private func writeLine(_ s: Int32, _ obj: [String: Any]) -> Bool {
    guard var data = try? JSONSerialization.data(withJSONObject: obj) else { return false }
    data.append(0x0a)
    return data.withUnsafeBytes { raw in
        var sent = 0
        while sent < raw.count {
            var pfd = pollfd(fd: s, events: Int16(POLLOUT), revents: 0)
            guard poll(&pfd, 1, 1000) == 1 else { return false }
            let n = send(s, raw.baseAddress!.advanced(by: sent), raw.count - sent, 0)
            if n < 0, errno == EAGAIN || errno == EINTR { continue }
            if n <= 0 { return false }
            sent += n
        }
        return true
    }
}

/// The first JSON line within `timeout`.
private func readLine(_ s: Int32, timeout: TimeInterval) -> [String: Any]? {
    let deadline = Date().addingTimeInterval(timeout)
    var buf = [UInt8](repeating: 0, count: 4096)
    var line = Data()
    while true {
        let left = deadline.timeIntervalSinceNow
        guard left > 0 else { return nil }
        var pfd = pollfd(fd: s, events: Int16(POLLIN), revents: 0)
        guard poll(&pfd, 1, Int32(left * 1000) + 1) == 1 else { return nil }
        let n = recv(s, &buf, buf.count, 0)
        if n < 0, errno == EAGAIN || errno == EINTR { continue }
        guard n > 0 else { return nil }
        line.append(contentsOf: buf[0..<n])
        if let nl = line.firstIndex(of: 0x0a) {
            return (try? JSONSerialization.jsonObject(with: line[line.startIndex..<nl])) as? [String: Any]
        }
        if line.count > Proto.maxLine { return nil }
    }
}

/// A receiver from its welcome: only a v0 welcome with an id counts.
public func receiverFrom(welcome w: [String: Any], ip: UInt32, port: UInt16, ifname: String, ifindex: UInt32) -> Receiver? {
    guard w["t"] as? String == "welcome", (w["v"] as? NSNumber)?.intValue == Int(Proto.version),
          let id = w["id"] as? String, !id.isEmpty else { return nil }
    return Receiver(id: id, name: w["name"] as? String ?? id, model: w["model"] as? String ?? "",
                    address: ipString(ip), interfaceName: ifname, interfaceIndex: ifindex, port: port, service: "")
}
