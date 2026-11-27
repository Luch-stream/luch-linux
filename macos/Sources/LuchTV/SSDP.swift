// SSDP (UPnP Device Architecture 1.1, section 1): finds DLNA renderers. An M-SEARCH goes to 239.255.255.250:1900;
// every renderer answers by unicast with the LOCATION of its device description, which lists its services.
// The same as luchd/src/tv/ssdp.rs.
// SPDX-License-Identifier: GPL-3.0-or-later
import Darwin
import Foundation
import LuchProto

public enum SSDP {
    public static let group = "239.255.255.250"
    public static let renderer = "urn:schemas-upnp-org:device:MediaRenderer:1"
    static let avTransport = "urn:schemas-upnp-org:service:AVTransport:"
    static let rendering = "urn:schemas-upnp-org:service:RenderingControl:"
    static let maxDescription = 256 * 1024

    /// Where to search and how far: the real group with the TTL the spec asks for (2) on every LAN interface, or, in
    /// tests, TTL 0 on loopback and a port of their own (nothing leaves this machine, no real TV answers).
    public struct Target {
        public var port: UInt16 = 1900
        public var ttl: UInt8 = 2
        /// IPv4 addresses of the interfaces to send from; nil: every LAN interface (a VPN's tunnel is left out).
        public var interfaces: [String]?

        public init(port: UInt16 = 1900, ttl: UInt8 = 2, interfaces: [String]? = nil) {
            self.port = port
            self.ttl = ttl
            self.interfaces = interfaces
        }
    }

    /// One answer to an M-SEARCH: the headers that matter.
    public struct Answer: Equatable {
        public var location: String
        public var usn: String
        public var st: String
    }

    public static func mSearch(_ st: String, mx: Int) -> String {
        "M-SEARCH * HTTP/1.1\r\nHOST: 239.255.255.250:1900\r\nMAN: \"ssdp:discover\"\r\nMX: \(mx)\r\nST: \(st)\r\nUSER-AGENT: macOS/1 UPnP/1.1 Luch\r\n\r\n"
    }

    /// The headers of an SSDP message, names lowercased; nil unless it is an answer (`HTTP/1.1 200`) or a NOTIFY.
    public static func parse(_ msg: String) -> [String: String]? {
        let lines = msg.components(separatedBy: "\r\n")
        guard let first = lines.first, first.hasPrefix("HTTP/1.1 200") || first.hasPrefix("NOTIFY ") else { return nil }
        var h: [String: String] = [:]
        for l in lines.dropFirst() {
            guard let colon = l.firstIndex(of: ":") else { continue }
            h[l[..<colon].trimmingCharacters(in: .whitespaces).lowercased()] = l[l.index(after: colon)...].trimmingCharacters(in: .whitespaces)
        }
        return h
    }

    /// IPv4 addresses of the interfaces a TV may be on: up, multicast, not loopback, not a tunnel.
    static func lanAddresses() -> [String] {
        var head: UnsafeMutablePointer<ifaddrs>?
        guard getifaddrs(&head) == 0, let first = head else { return [] }
        defer { freeifaddrs(head) }
        var out: [String] = []
        var cur: UnsafeMutablePointer<ifaddrs>? = first
        while let ifa = cur {
            defer { cur = ifa.pointee.ifa_next }
            let flags = Int32(ifa.pointee.ifa_flags)
            guard let sa = ifa.pointee.ifa_addr, sa.pointee.sa_family == UInt8(AF_INET),
                  flags & IFF_UP != 0, flags & IFF_MULTICAST != 0, flags & IFF_LOOPBACK == 0,
                  flags & IFF_POINTOPOINT == 0 else { continue }
            let name = String(cString: ifa.pointee.ifa_name)
            if ["utun", "ipsec", "ppp", "gif", "stf", "awdl", "llw"].contains(where: name.hasPrefix) { continue }
            var a = sa.withMemoryRebound(to: sockaddr_in.self, capacity: 1) { $0.pointee.sin_addr }
            var buf = [CChar](repeating: 0, count: Int(INET_ADDRSTRLEN))
            inet_ntop(AF_INET, &a, &buf, socklen_t(INET_ADDRSTRLEN))
            out.append(String(cString: buf))
        }
        return out
    }

    /// Sends an M-SEARCH for renderers and gathers the answers for `wait` seconds (one per device, by USN).
    public static func search(_ target: Target = Target(), wait: Double) throws -> [Answer] {
        let fd = socket(AF_INET, SOCK_DGRAM, 0)
        guard fd >= 0 else { throw TVError("SSDP socket: \(errnoString())") }
        defer { close(fd) }
        var ttl = target.ttl
        setsockopt(fd, IPPROTO_IP, IP_MULTICAST_TTL, &ttl, socklen_t(MemoryLayout<UInt8>.size))
        var loop: UInt8 = 1
        setsockopt(fd, IPPROTO_IP, IP_MULTICAST_LOOP, &loop, socklen_t(MemoryLayout<UInt8>.size))
        var to = sockaddr_in()
        to.sin_len = UInt8(MemoryLayout<sockaddr_in>.size)
        to.sin_family = sa_family_t(AF_INET)
        to.sin_port = target.port.bigEndian
        inet_pton(AF_INET, group, &to.sin_addr)
        let msg = Array(mSearch(renderer, mx: Int(min(5, max(1, wait.rounded())))).utf8)
        var sent = 0
        for ifAddress in target.interfaces ?? lanAddresses() {
            var a = in_addr()
            guard inet_pton(AF_INET, ifAddress, &a) == 1 else { continue }
            setsockopt(fd, IPPROTO_IP, IP_MULTICAST_IF, &a, socklen_t(MemoryLayout<in_addr>.size))
            // UDP may drop one; the spec suggests sending more than once.
            for _ in 0..<2 {
                let n = withUnsafePointer(to: &to) {
                    $0.withMemoryRebound(to: sockaddr.self, capacity: 1) {
                        sendto(fd, msg, msg.count, 0, $0, socklen_t(MemoryLayout<sockaddr_in>.size))
                    }
                }
                if n > 0 { sent += 1 }
            }
        }
        guard sent > 0 else { throw TVError("SSDP: no interface to search on") }
        var found: [Answer] = []
        var buf = [UInt8](repeating: 0, count: 4096)
        let until = now() + wait
        while true {
            let left = until - now()
            if left <= 0 { break }
            var p = pollfd(fd: fd, events: Int16(POLLIN), revents: 0)
            guard poll(&p, 1, Int32(left * 1000)) > 0 else { break }
            let n = recv(fd, &buf, buf.count, 0)
            guard n > 0, let h = parse(String(decoding: buf[0..<n], as: UTF8.self)),
                  let location = h["location"], let usn = h["usn"] else { continue }
            if !found.contains(where: { $0.usn == usn }) {
                found.append(Answer(location: location, usn: usn, st: h["st"] ?? ""))
            }
        }
        return found
    }

    /// Fetches a renderer's description and makes a device of it; nil unless it has an AVTransport.
    public static func describe(_ location: String) -> Result<Device?, TVError> {
        switch HTTP.call(location, timeout: 3) {
        case .failure(let e): return .failure(e)
        case .success(let a):
            guard (200..<300).contains(a.status) else { return .failure(TVError("\(location): HTTP \(a.status)")) }
            guard a.body.count <= maxDescription else { return .failure(TVError("\(location): description too long")) }
            return .success(device(location: location, description: a.text))
        }
    }

    /// The device a description makes, its control URLs made absolute (against URLBase, else the LOCATION).
    public static func device(location: String, description desc: String) -> Device? {
        let base = XML.text(desc, "URLBase").flatMap { $0.hasPrefix("http") ? $0 : nil } ?? location
        var av: (String, String)?
        var rc: (url: String, type: String)?
        for s in XML.all(desc, "service") {
            let s = String(s)
            let ty = XML.text(s, "serviceType") ?? ""
            guard let rel = XML.text(s, "controlURL") else { continue }
            let url = URL(string: rel, relativeTo: URL(string: base))?.absoluteURL.absoluteString ?? rel
            if ty.hasPrefix(avTransport), av == nil { av = (url, ty) }
            else if ty.hasPrefix(rendering), rc == nil { rc = (url, ty) }
        }
        guard let (avURL, avType) = av, let udn = XML.text(desc, "UDN"),
              let u = URL(string: location), let host = u.host else { return nil }
        var a = in_addr()
        guard inet_pton(AF_INET, host, &a) == 1 else { return nil }
        let model = [XML.text(desc, "manufacturer"), XML.text(desc, "modelName")].compactMap { $0 }
            .filter { !$0.isEmpty }.joined(separator: " ")
        let id = udn.hasPrefix("uuid:") ? String(udn.dropFirst(5)) : udn
        return Device(id: "dlna:\(id)", kind: .dlna, name: XML.text(desc, "friendlyName") ?? "TV", model: model,
                      address: host, port: UInt16(u.port ?? 80),
                      control: .dlna(avTransport: avURL, avType: avType, rendering: rc))
    }
}
