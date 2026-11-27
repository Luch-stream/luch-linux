// Asking every address of the LAN (PROTOCOL.md 1b), as luchd's find/scan.rs does: for networks that drop multicast
// (hotel and guest Wi-Fi, some mesh systems) a receiver is still a TCP port. Every address of the interface's
// subnet, the neighbours the system already knows first, then nearest to ours; bounded in flight and in all.
// SPDX-License-Identifier: GPL-3.0-or-later
import Foundation

public enum Sweep {
    /// At most this many addresses per interface: a /22. A bigger subnet (a hotel's /16) is cut to the /22 around
    /// us, where a DHCP server that hands out addresses in order most likely put the TV too.
    public static let maxHosts = 1024
    /// Connections in flight at once: an empty address costs a whole connect timeout, so this is what makes the
    /// sweep fast, and what keeps it from flooding a small router.
    public static let inFlight = 128
    /// The whole sweep is given up after this.
    public static let limit: TimeInterval = 20

    /// The addresses of `i`'s subnet (cut to maxHosts around ours), nearest to ours first, without ours, the
    /// network and the broadcast address.
    public static func targets(_ i: NetInterface) -> [UInt32] {
        let prefix = max(i.prefix, 32 - maxHosts.trailingZeroBitCount)
        guard prefix < 31 else { return [] }
        let mask = UInt32.max << UInt32(32 - prefix)
        let net = i.addr & mask
        let last = net | ~mask
        guard last > net + 1 else { return [] }
        return (net + 1..<last).filter { $0 != i.addr }.sorted {
            let (a, b) = (dist($0, i.addr), dist($1, i.addr))
            return a != b ? a < b : $0 < $1
        }
    }

    private static func dist(_ a: UInt32, _ b: UInt32) -> UInt32 { a > b ? a - b : b - a }

    /// Complete entries of `arp -an` on `ifname`: hosts known to be up, worth asking first. macOS prints
    /// "? (192.168.1.20) at aa:bb:cc:dd:ee:1 on en0 ifscope [ethernet]", and "(incomplete)" for none.
    public static func neighbours(_ arp: String, on ifname: String) -> [UInt32] {
        arpEntries(arp).filter { $0.ifname == ifname }.map(\.ip)
    }

    /// The order of the sweep: the known neighbours, then the rest of the subnet.
    public static func plan(_ i: NetInterface, arp: String) -> [UInt32] {
        let all = targets(i)
        let inSubnet = Set(all)
        var first: [UInt32] = []
        for a in neighbours(arp, on: i.name) where inSubnet.contains(a) && !first.contains(a) { first.append(a) }
        let seen = Set(first)
        return first + all.filter { !seen.contains($0) }
    }

    /// Runs `probe` on every target, at most `limit` at once, each find handed to `found` as soon as it is made
    /// (on a worker thread); returns them all once every target is asked or `deadline` passes.
    public static func run<A, T>(_ targets: [A], limit: Int = inFlight, deadline: TimeInterval = Sweep.limit,
                                 probe: @escaping (A) -> T?, found: @escaping (T) -> Void = { _ in }) -> [T] {
        let lock = NSLock()
        var next = 0
        var out: [T] = []
        let done = DispatchGroup()
        let until = Date().addingTimeInterval(deadline)
        for _ in 0..<min(limit, targets.count) {
            done.enter()
            let t = Thread {
                defer { done.leave() }
                while Date() < until {
                    lock.lock()
                    let i = next
                    next += 1
                    lock.unlock()
                    guard i < targets.count else { return }
                    if let hit = probe(targets[i]) {
                        lock.lock()
                        out.append(hit)
                        lock.unlock()
                        found(hit)
                    }
                }
            }
            t.stackSize = 256 * 1024
            t.start()
        }
        _ = done.wait(timeout: .now() + deadline + ProbeTimes.welcome + 1)
        lock.lock()
        defer { lock.unlock() }
        return out
    }
}

/// One line of `arp -an`: a complete entry.
public struct ArpEntry: Equatable {
    public var ip: UInt32
    /// "aa:bb:cc:dd:ee:01": macOS's single-digit octets padded.
    public var mac: String
    public var ifname: String
}

public func arpEntries(_ table: String) -> [ArpEntry] {
    table.split(separator: "\n").compactMap { line in
        let f = line.split(separator: " ").map(String.init)
        guard f.count >= 6, f[2] == "at", f[4] == "on", f[1].hasPrefix("("), f[1].hasSuffix(")"),
              let ip = parseIPv4(String(f[1].dropFirst().dropLast())) else { return nil }
        let parts = f[3].split(separator: ":", omittingEmptySubsequences: false)
        guard parts.count == 6, parts.allSatisfy({ (1...2).contains($0.count) && UInt8($0, radix: 16) != nil }) else {
            return nil
        }
        let mac = parts.map { $0.count == 1 ? "0" + $0.lowercased() : $0.lowercased() }.joined(separator: ":")
        guard mac != "00:00:00:00:00:00", mac != "ff:ff:ff:ff:ff:ff" else { return nil }
        return ArpEntry(ip: ip, mac: mac, ifname: f[5])
    }
}

/// The system's neighbour table, as `arp -an` prints it ("" when it cannot be read).
public func arpTable() -> String {
    let p = Process()
    p.executableURL = URL(fileURLWithPath: "/usr/sbin/arp")
    p.arguments = ["-an"]
    let out = Pipe()
    p.standardOutput = out
    p.standardError = FileHandle.nullDevice
    guard (try? p.run()) != nil else { return "" }
    let data = out.fileHandleForReading.readDataToEndOfFile()
    p.waitUntilExit()
    return String(decoding: data, as: UTF8.self)
}
