// The sweep of the LAN and the probe (PROTOCOL.md 1b, 2) with luchd's vectors (find/scan.rs, find/mod.rs).
// SPDX-License-Identifier: GPL-3.0-or-later
import Darwin
@testable import LuchFind
@testable import LuchNet
import XCTest

final class SweepTests: XCTestCase {
    let up = UInt32(IFF_UP | IFF_RUNNING)

    func ip(_ a: UInt32, _ b: UInt32, _ c: UInt32, _ d: UInt32) -> UInt32 { a << 24 | b << 16 | c << 8 | d }

    func wifi(_ addr: UInt32, _ prefix: Int, name: String = "en0", flags: UInt32? = nil) -> NetInterface {
        NetInterface(name: name, index: 4, addr: addr, prefix: prefix, flags: flags ?? up)
    }

    func testVPNsAndVirtualBridgesAreNeverScanned() {
        XCTAssertTrue(scannable(wifi(ip(192, 168, 1, 10), 24)))
        let p2p = up | UInt32(IFF_POINTOPOINT)
        XCTAssertFalse(scannable(wifi(ip(10, 8, 0, 2), 24, name: "utun3", flags: p2p)))
        XCTAssertFalse(scannable(wifi(ip(10, 8, 0, 2), 24, name: "anything", flags: p2p)))
        XCTAssertFalse(scannable(wifi(ip(100, 64, 0, 2), 24, name: "utun4")), "a tunnel by name")
        XCTAssertFalse(scannable(wifi(ip(10, 8, 0, 2), 24, name: "ipsec0")))
        XCTAssertFalse(scannable(wifi(ip(192, 168, 64, 1), 24, name: "bridge100")), "a virtual machine's network")
        XCTAssertFalse(scannable(wifi(ip(127, 0, 0, 1), 8, name: "lo0", flags: up | UInt32(IFF_LOOPBACK))))
        XCTAssertFalse(scannable(wifi(ip(192, 168, 1, 10), 24, flags: UInt32(IFF_UP))), "no carrier")
        XCTAssertTrue(scannable(wifi(ip(172, 16, 0, 5), 16, name: "en7")))
    }

    func testA24IsEveryOtherHostNearestFirst() {
        let t = Sweep.targets(wifi(ip(192, 168, 1, 10), 24))
        XCTAssertEqual(t.count, 253)
        XCTAssertEqual(Array(t[..<4]), [ip(192, 168, 1, 9), ip(192, 168, 1, 11), ip(192, 168, 1, 8), ip(192, 168, 1, 12)])
        XCTAssertFalse(t.contains(ip(192, 168, 1, 10)))
        XCTAssertFalse(t.contains(ip(192, 168, 1, 0)))
        XCTAssertFalse(t.contains(ip(192, 168, 1, 255)))
    }

    func testABigSubnetIsCutToThe22AroundUs() {
        let t = Sweep.targets(wifi(ip(10, 20, 130, 7), 16))
        XCTAssertEqual(t.count, Sweep.maxHosts - 3)
        XCTAssertTrue(t.allSatisfy { (128..<132).contains($0 >> 8 & 255) })
        XCTAssertTrue(Sweep.targets(wifi(ip(10, 0, 0, 1), 31)).isEmpty)
        XCTAssertEqual(Sweep.targets(wifi(ip(10, 0, 0, 1), 30)), [ip(10, 0, 0, 2)])
    }

    /// macOS's `arp -an`: octets without their leading zero, incomplete entries, other interfaces.
    let arp = """
    ? (192.168.1.200) at aa:bb:cc:dd:ee:1 on en0 ifscope [ethernet]
    ? (192.168.1.201) at (incomplete) on en0 ifscope [ethernet]
    ? (10.8.0.1) at aa:bb:cc:dd:ee:2 on utun3 ifscope [ethernet]
    ? (192.168.1.1) at aa:bb:cc:dd:ee:3 on en0 ifscope [ethernet]
    ? (192.168.1.255) at ff:ff:ff:ff:ff:ff on en0 ifscope [ethernet]
    """

    func testKnownNeighboursGoFirst() {
        let p = Sweep.plan(wifi(ip(192, 168, 1, 10), 24), arp: arp)
        XCTAssertEqual(Array(p[..<3]), [ip(192, 168, 1, 200), ip(192, 168, 1, 1), ip(192, 168, 1, 9)])
        XCTAssertEqual(p.count, 253)
        XCTAssertEqual(arpMac(arp, "192.168.1.200"), "aa:bb:cc:dd:ee:01")
        XCTAssertNil(arpMac(arp, "192.168.1.201"))
        XCTAssertNil(arpMac(arp, "192.168.1.2"))
    }

    func testTheInterfaceForAnAddress() {
        let ifs = [wifi(ip(192, 168, 1, 10), 24),
                   wifi(ip(10, 8, 0, 2), 8, name: "utun3", flags: up | UInt32(IFF_POINTOPOINT)),
                   wifi(ip(172, 16, 0, 5), 16, name: "en7")]
        XCTAssertEqual(interfaceFor(ip(192, 168, 1, 20), in: ifs)?.name, "en0")
        XCTAssertEqual(interfaceFor(ip(172, 16, 9, 9), in: ifs)?.name, "en7")
        // Inside the VPN's subnet, but a receiver is never reached through it: the LAN is used instead.
        XCTAssertEqual(interfaceFor(ip(10, 1, 2, 3), in: ifs)?.name, "en0")
        XCTAssertEqual(lanInterface(Array(ifs.dropFirst()))?.name, "en7")
    }

    func testThisMacHasInterfaces() {
        XCTAssertTrue(netInterfaces().contains { $0.addr == ip(127, 0, 0, 1) && $0.prefix == 8 && !scannable($0) })
    }

    func testTheSweepIsBoundedAndYieldsAsItGoes() {
        let lock = NSLock()
        var now = 0, peak = 0
        var live: [Int] = []
        let t0 = Date()
        let found = Sweep.run(Array(0..<400), limit: 40, probe: { a -> Int? in
            lock.lock()
            now += 1
            peak = max(peak, now)
            lock.unlock()
            // Most addresses are empty and cost a whole timeout; a few answer.
            Thread.sleep(forTimeInterval: a % 100 == 7 ? 0.005 : 0.05)
            lock.lock()
            now -= 1
            lock.unlock()
            return a % 100 == 7 ? a : nil
        }, found: { a in
            lock.lock()
            live.append(a)
            lock.unlock()
        })
        XCTAssertEqual(found.sorted(), [7, 107, 207, 307])
        XCTAssertEqual(live.sorted(), [7, 107, 207, 307])
        XCTAssertEqual(peak, 40)
        // 400 addresses, 40 at a time, 50 ms each: 0.5 s, not 20.
        XCTAssertLessThan(Date().timeIntervalSince(t0), 2)
    }

    let me = ProbeIdentity(id: "sender", name: "Test Mac")
    let loopback = UInt32(127) << 24 | 1

    /// The sweep with the real probe over real sockets: two receivers, something else on the port, a silent
    /// address, and many addresses with nothing.
    func testTheSweepFindsReceiversWithTheRealHandshake() {
        let a = LoopbackReceiver(id: "3f0c2a8e-9d1b-4c7e-8a55-0b6f2d9e1c47", name: "Living room TV")
        let b = LoopbackReceiver(id: "9a1b2c3d-0000-4000-8000-000000000001", name: "Bedroom TV")
        let junk = LoopbackReceiver(kind: .junk)
        let silent = LoopbackReceiver(kind: .silent)
        var ports: [UInt16] = (1...200).map { UInt16($0) }
        ports += [a.port, junk.port, silent.port, b.port]
        let t0 = Date()
        let got = Sweep.run(ports, limit: 64) { [me, loopback] port -> String? in
            guard let w = probe(loopback, port, ifindex: 0, me: me) else { return nil }
            return receiverFrom(welcome: w, ip: loopback, port: port, ifname: "lo0", ifindex: 1)?.name
        }
        XCTAssertEqual(got.sorted(), ["Bedroom TV", "Living room TV"])
        // The silent one costs the welcome timeout, not more.
        XCTAssertLessThan(Date().timeIntervalSince(t0), ProbeTimes.welcome + 1)
        _ = silent
    }

    func testAProbeCarriesTheTokenAndSaysItIsAProbe() {
        let a = LoopbackReceiver(id: "3f0c2a8e-9d1b-4c7e-8a55-0b6f2d9e1c47", name: "Living room TV")
        let w = probe(loopback, a.port, ifindex: 0, me: me, token: "0badf00d")!
        XCTAssertEqual(w["probe"] as? Bool, true)
        XCTAssertEqual(w["token"] as? String, "ok")
        let plain = probe(loopback, a.port, ifindex: 0, me: me)!
        XCTAssertNil(plain["token"])
        let r = receiverFrom(welcome: plain, ip: loopback, port: a.port, ifname: "lo0", ifindex: 1)!
        XCTAssertEqual([r.name, r.model, r.service, r.address], ["Living room TV", "Test TV", "", "127.0.0.1"])
        XCTAssertNil(receiverFrom(welcome: ["t": "welcome", "v": 1, "id": "x"], ip: loopback, port: 1, ifname: "lo0", ifindex: 1))
        XCTAssertNil(receiverFrom(welcome: ["t": "error"], ip: loopback, port: 1, ifname: "lo0", ifindex: 1))
        // A probe says bye and goes.
        Thread.sleep(forTimeInterval: 0.1)
        XCTAssertEqual(a.requests.last?["t"] as? String, "bye")
    }

    /// `add`: a receiver by its link (with the token the QR code gave) and by its address; a link to another TV than
    /// the one named is refused.
    func testAReceiverIsAddedByLinkOrAddress() throws {
        let a = LoopbackReceiver(id: "3f0c2a8e-9d1b-4c7e-8a55-0b6f2d9e1c47", name: "Living room TV")
        let known = KnownReceivers(file: FileManager.default.temporaryDirectory.appendingPathComponent("luch-add-\(getpid()).json"))
        let finder = Finder(me: me, known: known) { [] }
        XCTAssertEqual(try finder.add(link: "luch://connect?host=127.0.0.1&port=\(a.port)&t=0badf00d"),
                       "3f0c2a8e-9d1b-4c7e-8a55-0b6f2d9e1c47")
        XCTAssertEqual(finder.token(for: "3f0c2a8e-9d1b-4c7e-8a55-0b6f2d9e1c47"), "0badf00d")
        XCTAssertEqual(finder.discovered()["3f0c2a8e-9d1b-4c7e-8a55-0b6f2d9e1c47"]?["via"] as? [String], ["qr"])
        XCTAssertEqual(try finder.add(link: "127.0.0.1:\(a.port)"), "3f0c2a8e-9d1b-4c7e-8a55-0b6f2d9e1c47")
        XCTAssertThrowsError(try finder.add(link: "luch://connect?host=127.0.0.1&port=\(a.port)&id=9a1b2c3d"))
        XCTAssertThrowsError(try finder.add(link: "tv.local"))
        XCTAssertEqual(finder.list().map(\.name), ["Living room TV"])
        // What the receiver said of itself, for `info`.
        let info = finder.info(finder.list()[0])
        XCTAssertEqual(info["android"] as? String, "Android 5.1")
        XCTAssertEqual(info["audio_latency_ms"] as? Int, 120)
        let limits = info["limits"] as? [String] ?? []
        XCTAssertTrue(limits.first?.contains("120 ms") == true, "\(limits)")
        XCTAssertEqual(limits.last, "x_new")
    }
}
