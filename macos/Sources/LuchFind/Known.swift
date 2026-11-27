// Every receiver this Mac has talked to, and how each can be turned on from standby, as luchd's wake.rs keeps it
// (~/Library/Application Support/Luch/known.json, the same fields): a receiver that is off is not on the LAN.
//
// - `mac`: for a Wake-on-LAN magic packet, learned from the neighbour table while it was on.
// - `ble`: the Bluetooth LE advertisement a MediaTek TV remote sends when its power key is pressed (company id
//   0x0046); XGIMI projectors and others switch Wi-Fi off in standby and wake only on it.
// SPDX-License-Identifier: GPL-3.0-or-later
import Foundation
import LuchNet

public final class KnownReceivers {
    private let file: URL
    private let lock = NSLock()

    public init(file: URL) {
        self.file = file
    }

    public static func defaultFile() -> URL {
        let dir = FileManager.default.homeDirectoryForCurrentUser
            .appendingPathComponent("Library/Application Support/Luch", isDirectory: true)
        try? FileManager.default.createDirectory(at: dir, withIntermediateDirectories: true)
        return dir.appendingPathComponent("known.json")
    }

    public func load() -> [String: [String: Any]] {
        lock.lock()
        defer { lock.unlock() }
        return read()
    }

    private func read() -> [String: [String: Any]] {
        guard let data = try? Data(contentsOf: file),
              let all = (try? JSONSerialization.jsonObject(with: data)) as? [String: [String: Any]] else { return [:] }
        return all
    }

    private func write(_ all: [String: [String: Any]]) {
        guard let data = try? JSONSerialization.data(withJSONObject: all, options: [.sortedKeys]) else { return }
        try? data.write(to: file, options: .atomic)
    }

    /// Notes a receiver just talked to: its name, model, address and its MAC (from the neighbour table the
    /// handshake has just filled).
    public func remember(id: String, name: String, model: String, address: String, mac: String?) {
        lock.lock()
        defer { lock.unlock() }
        var all = read()
        KnownReceivers.merge(&all, id: id, name: name, model: model, address: address, mac: mac,
                             now: Int(Date().timeIntervalSince1970))
        write(all)
    }

    /// Sets (or with "" clears) a receiver's `mac` or `ble`, checked first.
    public func set(id: String, kind: String, value: String) throws {
        lock.lock()
        defer { lock.unlock() }
        var all = read()
        try KnownReceivers.set(&all, id: id, kind: kind, value: value)
        write(all)
    }

    static func merge(_ all: inout [String: [String: Any]], id: String, name: String, model: String, address: String,
                      mac: String?, now: Int) {
        // A receiver app that was reinstalled comes back under a new id: the same device (MAC) with the same name.
        // Its old entry goes, and what was learned for waking it moves over.
        var carried: [String: Any] = [:]
        if let mac {
            let old = all.filter { $0.key != id && $0.value["mac"] as? String == mac && $0.value["name"] as? String == name }
            for (k, o) in old {
                all.removeValue(forKey: k)
                if let ble = o["ble"] { carried["ble"] = ble }
            }
        }
        var r = all[id] ?? [:]
        for (k, v) in carried where r[k] == nil { r[k] = v }
        r["name"] = name
        r["model"] = model
        r["address"] = address
        if let mac { r["mac"] = mac }
        r["seen"] = now
        all[id] = r
    }

    static func set(_ all: inout [String: [String: Any]], id: String, kind: String, value: String) throws {
        let normal: String?
        switch (kind, value) {
        case (_, ""): normal = nil
        case ("mac", let v):
            guard let m = parseMac(v) else { throw FindError("not a MAC address: \(v)") }
            normal = formatMac(m)
        case ("ble", let v):
            guard let b = parseHex(v), (2...26).contains(b.count) else {
                throw FindError("not the hex bytes of an advertisement: \(v)")
            }
            normal = hexString(b)
        default:
            throw FindError("unknown wake setting \(kind) (mac or ble)")
        }
        guard var r = all[id] else { throw FindError("unknown receiver \(id): connect to it once first") }
        r[kind] = normal
        all[id] = r
    }

    /// The ways a known receiver can be woken.
    public static func methods(_ r: [String: Any]) -> [String] {
        (r["mac"] is String ? ["wol"] : []) + (r["ble"] is String ? ["ble"] : [])
    }

    /// What to tell the person about a receiver that cannot be woken yet.
    public static func hint(_ r: [String: Any]) -> String? {
        guard !(r["ble"] is String) else { return nil }
        let model = (r["model"] as? String ?? "").lowercased()
        guard model.contains("xgimi") else { return nil }
        return "leaves the network in standby: turn it off, run `luchctl wake <r> learn` and press the remote's power key"
    }
}

// MARK: - Wake-on-LAN

public func parseMac(_ s: String) -> [UInt8]? {
    let parts = s.trimmingCharacters(in: .whitespaces).split(omittingEmptySubsequences: false) { $0 == ":" || $0 == "-" }
    guard parts.count == 6 else { return nil }
    var mac: [UInt8] = []
    for p in parts {
        guard p.count == 2, let b = UInt8(p, radix: 16) else { return nil }
        mac.append(b)
    }
    return mac != [0, 0, 0, 0, 0, 0] && mac != [255, 255, 255, 255, 255, 255] ? mac : nil
}

public func formatMac(_ m: [UInt8]) -> String {
    m.map { String(format: "%02x", $0) }.joined(separator: ":")
}

/// The MAC of `ip` in `arp -an`, if the entry is complete.
public func arpMac(_ table: String, _ ip: String) -> String? {
    guard let a = parseIPv4(ip) else { return nil }
    return arpEntries(table).first { $0.ip == a }?.mac
}

/// Six 0xff and the MAC sixteen times.
public func magicPacket(_ mac: [UInt8]) -> [UInt8] {
    [UInt8](repeating: 0xff, count: 6) + (0..<16).flatMap { _ in mac }
}

/// Where the magic packet goes: the limited broadcast and the subnet's own broadcast, on the LAN that holds the
/// receiver's last address (else the main LAN), never through a VPN.
public func wolTargets(lastAddress: String?, ifaces: [NetInterface]) -> (iface: NetInterface, to: [UInt32])? {
    let ip = lastAddress.flatMap(parseIPv4)
    guard let lan = ip.flatMap({ interfaceFor($0, in: ifaces) }) ?? lanInterface(ifaces) else { return nil }
    var to: [UInt32] = [UInt32.max, lan.addr | ~lan.mask]
    // An address of another subnet (the receiver was seen elsewhere): its /24 broadcast too, as luchd does.
    if let ip, !lan.holds(ip) { to.append(ip | 0xff) }
    return (lan, to)
}

public func sendWakeOnLan(mac: [UInt8], lastAddress: String?) throws {
    guard let (iface, targets) = wolTargets(lastAddress: lastAddress, ifaces: netInterfaces()) else {
        throw FindError("no LAN interface for a broadcast")
    }
    let s = socket(AF_INET, SOCK_DGRAM, 0)
    guard s >= 0 else { throw FindError("no socket") }
    defer { close(s) }
    var on: Int32 = 1
    setsockopt(s, SOL_SOCKET, SO_BROADCAST, &on, socklen_t(MemoryLayout<Int32>.size))
    // Bound to the LAN: a full-tunnel VPN would otherwise take the limited broadcast too.
    var idx = iface.index
    setsockopt(s, IPPROTO_IP, IP_BOUND_IF, &idx, socklen_t(MemoryLayout<UInt32>.size))
    let packet = magicPacket(mac)
    var sent = false
    for t in targets {
        for port: UInt16 in [9, 7] {
            var a = sockaddr_in()
            a.sin_len = UInt8(MemoryLayout<sockaddr_in>.size)
            a.sin_family = sa_family_t(AF_INET)
            a.sin_port = port.bigEndian
            a.sin_addr.s_addr = t.bigEndian
            let n = withUnsafePointer(to: &a) {
                $0.withMemoryRebound(to: sockaddr.self, capacity: 1) {
                    sendto(s, packet, packet.count, 0, $0, socklen_t(MemoryLayout<sockaddr_in>.size))
                }
            }
            sent = sent || n == packet.count
        }
    }
    if !sent { throw FindError("no route for a broadcast on \(iface.name)") }
}
