// This Mac's IPv4 interfaces and which of them are a LAN with a TV on it (PROTOCOL.md 1b, 2): the same rules as
// luchd's find/scan.rs, so a VPN's utun is never asked for a receiver and never carries a stream.
// SPDX-License-Identifier: GPL-3.0-or-later
import Darwin
import Foundation

public struct NetInterface: Equatable {
    public var name: String
    public var index: UInt32
    /// Host order.
    public var addr: UInt32
    public var prefix: Int
    public var flags: UInt32

    public init(name: String, index: UInt32, addr: UInt32, prefix: Int, flags: UInt32) {
        self.name = name
        self.index = index
        self.addr = addr
        self.prefix = prefix
        self.flags = flags
    }

    public var mask: UInt32 { prefix == 0 ? 0 : UInt32.max << (32 - UInt32(prefix)) }

    /// Whether `ip` (host order) is on this interface's subnet.
    public func holds(_ ip: UInt32) -> Bool { addr & mask == ip & mask }

    public var isLoopback: Bool { flags & UInt32(IFF_LOOPBACK) != 0 }
}

/// Names that are never a LAN with a TV on it: VPNs and other tunnels (utun is also point-to-point, a tap or a
/// Tailscale interface may not be), and the bridges of virtual machines and Internet Sharing on this Mac.
private let notALan = ["utun", "tun", "tap", "ipsec", "ppp", "wg", "tailscale", "zt", "gif", "stf", "awdl", "llw",
                       "bridge", "vmnet", "vboxnet", "anpi", "docker", "veth"]

/// Section 2: never through a VPN. Up, running, not loopback, not point-to-point, not a tunnel or a bridge by name.
public func scannable(_ i: NetInterface) -> Bool {
    let up = i.flags & UInt32(IFF_UP) != 0 && i.flags & UInt32(IFF_RUNNING) != 0
    let odd = i.flags & UInt32(IFF_LOOPBACK | IFF_POINTOPOINT) != 0
    return up && !odd && i.prefix >= 8 && !notALan.contains { i.name.hasPrefix($0) }
}

/// Dotted quad of a host-order address.
public func ipString(_ a: UInt32) -> String {
    "\(a >> 24).\(a >> 16 & 255).\(a >> 8 & 255).\(a & 255)"
}

/// Host-order address of a dotted quad, nil for anything else.
public func parseIPv4(_ s: String) -> UInt32? {
    let parts = s.split(separator: ".", omittingEmptySubsequences: false)
    guard parts.count == 4 else { return nil }
    var out: UInt32 = 0
    for p in parts {
        guard !p.isEmpty, p.count <= 3, p.allSatisfy(\.isNumber), let v = UInt32(p), v <= 255 else { return nil }
        out = out << 8 | v
    }
    return out
}

/// This Mac's IPv4 interfaces.
public func netInterfaces() -> [NetInterface] {
    var head: UnsafeMutablePointer<ifaddrs>?
    guard getifaddrs(&head) == 0, let first = head else { return [] }
    defer { freeifaddrs(head) }
    var out: [NetInterface] = []
    var cur: UnsafeMutablePointer<ifaddrs>? = first
    while let ifa = cur {
        defer { cur = ifa.pointee.ifa_next }
        guard let sa = ifa.pointee.ifa_addr, let nm = ifa.pointee.ifa_netmask,
              sa.pointee.sa_family == UInt8(AF_INET) else { continue }
        let addr = sa.withMemoryRebound(to: sockaddr_in.self, capacity: 1) { UInt32(bigEndian: $0.pointee.sin_addr.s_addr) }
        let mask = nm.withMemoryRebound(to: sockaddr_in.self, capacity: 1) { UInt32(bigEndian: $0.pointee.sin_addr.s_addr) }
        let name = String(cString: ifa.pointee.ifa_name)
        out.append(NetInterface(name: name, index: if_nametoindex(ifa.pointee.ifa_name), addr: addr,
                                prefix: (~mask).leadingZeroBitCount, // the leading ones of the mask
                                flags: ifa.pointee.ifa_flags))
    }
    return out
}

/// The LAN this Mac is on: Ethernet or Wi-Fi (en*) first, never a tunnel.
public func lanInterface(_ ifaces: [NetInterface] = netInterfaces()) -> NetInterface? {
    let lans = ifaces.filter(scannable)
    return lans.first { $0.name.hasPrefix("en") } ?? lans.first
}

/// The interface to reach `ip` from: the LAN whose subnet holds it, else the main LAN (a receiver behind a router),
/// never a tunnel, even when the VPN's subnet holds the address.
public func interfaceFor(_ ip: UInt32, in ifaces: [NetInterface] = netInterfaces()) -> NetInterface? {
    let loopback = ip >> 24 == 127
    let usable = ifaces.filter { scannable($0) || ($0.isLoopback && loopback) }
    return usable.first { $0.holds(ip) } ?? lanInterface(ifaces)
}

/// Interface index of the LAN (PROTOCOL.md 2), 0 when there is none.
public func lanInterfaceIndex() -> UInt32 {
    lanInterface()?.index ?? 0
}
