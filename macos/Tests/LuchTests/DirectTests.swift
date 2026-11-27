// Connecting directly: the card's PSM (the vector all three sides test), the Bluetooth channel bridged to loopback,
// the receiver's answer, and the Wi-Fi left and come back to, with a fake Wi-Fi and a fake channel; end to end
// against the Swift receiver (FakeTLSReceiver), pairing included.
// SPDX-License-Identifier: GPL-3.0-or-later
import Darwin
import Foundation
@testable import LuchFind
@testable import LuchNet
import LuchProto
import XCTest

/// The Mac's Wi-Fi, in memory: what was asked of it, in order.
final class FakeWiFi: WiFiClient {
    var ssid: String?
    var interfaceName: String? = "en0"
    var namesAllowed = true
    var joinFails: String?
    var rejoinFails = false
    private(set) var calls: [String] = []

    init(on ssid: String?) { self.ssid = ssid }

    func allowNames() -> Bool {
        calls.append("allow")
        return namesAllowed
    }

    func join(ssid: String, password: String) throws {
        calls.append("join \(ssid) \(password)")
        if let joinFails { throw FindError(joinFails) }
        self.ssid = ssid
    }

    func leave() {
        calls.append("leave")
        ssid = nil
    }

    func rejoin(ssid: String?) throws {
        calls.append("rejoin \(ssid ?? "-")")
        if rejoinFails, let ssid { throw FindError("could not go back to '\(ssid)': choose it in the Wi-Fi menu") }
        self.ssid = ssid
    }
}

/// Two bound stream pairs: what an L2CAP channel's streams are to the bridge, and the receiver's end of them.
struct FakeChannelEnds {
    let mine: (input: InputStream, output: OutputStream)
    let theirs: (input: InputStream, output: OutputStream)

    init() {
        var aIn: InputStream?, aOut: OutputStream?, bIn: InputStream?, bOut: OutputStream?
        Stream.getBoundStreams(withBufferSize: 65536, inputStream: &aIn, outputStream: &aOut)
        Stream.getBoundStreams(withBufferSize: 65536, inputStream: &bIn, outputStream: &bOut)
        mine = (aIn!, bOut!)
        theirs = (bIn!, aOut!)
        theirs.input.open()
        theirs.output.open()
    }

    /// The receiver's end, blocking.
    func read() -> [UInt8] {
        var buf = [UInt8](repeating: 0, count: 4096)
        let n = theirs.input.read(&buf, maxLength: buf.count)
        return n > 0 ? Array(buf[0..<n]) : []
    }

    func write(_ b: [UInt8]) { _ = theirs.output.write(b, maxLength: b.count) }
}

/// Channels to the receiver over the air, here a pipe to a TLS port on loopback (the receiver's side of PROTOCOL's
/// "feeds the L2CAP stream into the same TLS control server").
final class FakeChannels: DirectChannelOpener {
    let tlsPort: UInt16
    private(set) var opened = 0

    init(tlsPort: UInt16) { self.tlsPort = tlsPort }

    func open(_ heard: Heard, timeout: TimeInterval) throws -> DirectChannel {
        opened += 1
        let ends = FakeChannelEnds()
        guard let s = tcpConnect(0x7f00_0001, tlsPort, ifindex: 0, timeout: 2) else { throw FindError("no receiver") }
        _ = fcntl(s, F_SETFL, fcntl(s, F_GETFL) & ~O_NONBLOCK)
        Thread.detachNewThread {
            var buf = [UInt8](repeating: 0, count: 16384)
            while true {
                let n = recv(s, &buf, buf.count, 0)
                guard n > 0 else { break }
                ends.write(Array(buf[0..<n]))
            }
            ends.theirs.output.close()
        }
        Thread.detachNewThread {
            while true {
                let b = ends.read()
                guard !b.isEmpty else { break }
                _ = b.withUnsafeBytes { send(s, $0.baseAddress!, b.count, 0) }
            }
            shutdown(s, SHUT_RDWR)
            close(s)
        }
        return DirectChannel(input: ends.mine.input, output: ends.mine.output, close: {})
    }
}

final class DirectCardTests: XCTestCase {
    func testTheCardCarriesThePSM() {
        let vector: [UInt8] = [0x4c, 0x75, 0x01, 0x0b, 0x6f, 0x1c, 0x2a, 0, 0, 0, 0, 0, 0, 0x00, 0x80]
        let c = Card.decode(vector, bluetooth: true)
        XCTAssertEqual(c?.id, "0b6f1c2a")
        XCTAssertEqual(c?.psm, 0x0080)
        XCTAssertNil(c?.addr, "0.0.0.0:0: no address, reached directly only")
        XCTAssertEqual(c.map { $0.encode() }, vector)
        XCTAssertEqual(Card(kind: Card.receiver, version: 0, id: "0b6f1c2a", addr: nil, psm: 0x80).encode(), vector)
        // In an advertisement: CoreBluetooth hands the company id first, little-endian.
        XCTAssertEqual(BLE.card(Data([0xff, 0xff] + vector))?.psm, 0x80)
        // 15 bytes in the advert: flags 3 + manufacturer data 4 + 15 = 22, within 31.
        XCTAssertLessThanOrEqual(3 + 4 + vector.count, 31)

        // An address and a PSM; a sender reading only 13 bytes keeps the address.
        var both = Card(kind: Card.receiver, version: 0, id: "0b6f1c2a", addr: (tv192, 47800), psm: 0x0081)
        XCTAssertEqual(Card.decode(both.encode(), bluetooth: true), both)
        XCTAssertEqual(Card.decode(Array(both.encode().prefix(13)), bluetooth: true)?.addr?.port, 47800)
        both.psm = 0
        XCTAssertEqual(both.encode().count, 13, "no PSM: the card stays as it was")

        // By sound bytes 13.. are the token: no PSM there, and without an address the card is no use.
        XCTAssertNil(Card.decode(vector))
        XCTAssertNil(Card.decode([0x4c, 0x75, 0x01, 0x0b, 0x6f, 0x1c, 0x2a, 0, 0, 0, 0, 0, 0, 0, 0], bluetooth: true),
                     "neither an address nor a PSM")
    }
}

final class StreamBridgeTests: XCTestCase {
    func testBytesGoBothWaysAndAnEndEndsTheOtherSide() throws {
        let ends = FakeChannelEnds()
        let bridge = try StreamBridge(input: ends.mine.input, output: ends.mine.output, acceptWithin: 5)
        let ended = expectation(description: "the pipe ended")
        bridge.onEnd = { ended.fulfill() }
        guard let s = tcpConnect(0x7f00_0001, bridge.port, ifindex: 0, timeout: 2) else { return XCTFail("no bridge") }
        _ = fcntl(s, F_SETFL, fcntl(s, F_GETFL) & ~O_NONBLOCK)
        defer { close(s) }

        // TCP to the channel: a TLS ClientHello's worth, more than one write of the stream.
        let out = (0..<20000).map { UInt8($0 % 251) }
        XCTAssertEqual(out.withUnsafeBytes { send(s, $0.baseAddress!, out.count, 0) }, out.count)
        var got: [UInt8] = []
        while got.count < out.count {
            let b = ends.read()
            guard !b.isEmpty else { break }
            got += b
        }
        XCTAssertEqual(got, out)

        // The channel to TCP.
        ends.write(Array("welcome\n".utf8))
        var buf = [UInt8](repeating: 0, count: 64)
        var tv = timeval(tv_sec: 2, tv_usec: 0)
        setsockopt(s, SOL_SOCKET, SO_RCVTIMEO, &tv, socklen_t(MemoryLayout<timeval>.size))
        let n = recv(s, &buf, buf.count, 0)
        XCTAssertEqual(String(decoding: buf[0..<max(0, n)], as: UTF8.self), "welcome\n")

        // The receiver closes the channel: the TCP side sees the end.
        ends.theirs.output.close()
        XCTAssertEqual(recv(s, &buf, buf.count, 0), 0)
        wait(for: [ended], timeout: 3)
    }

    func testNobodyConnectingEndsIt() throws {
        let ends = FakeChannelEnds()
        let bridge = try StreamBridge(input: ends.mine.input, output: ends.mine.output, acceptWithin: 0.3)
        let ended = expectation(description: "given up")
        bridge.onEnd = { ended.fulfill() }
        wait(for: [ended], timeout: 3)
        XCTAssertNil(tcpConnect(0x7f00_0001, bridge.port, ifindex: 0, timeout: 0.5), "nobody else is let in")
    }
}

final class DirectAnswerTests: XCTestCase {
    let ok: [String: Any] = ["t": "direct", "ssid": "DIRECT-xy-Luch", "pass": "k3y-of-twelve", "freq": 5180,
                             "addr": "192.168.49.1", "port": 47800, "tls_port": 47802]

    func testTheGroupIsReadAndCheckedForWPA2() throws {
        XCTAssertEqual(try DirectAnswer.parse(ok, tv: "TV"),
                       DirectAnswer(ssid: "DIRECT-xy-Luch", pass: "k3y-of-twelve", freq: 5180, addr: "192.168.49.1",
                                    port: 47800, tlsPort: 47802))
        var noFreq = ok
        noFreq["freq"] = nil
        XCTAssertEqual(try DirectAnswer.parse(noFreq, tv: "TV").freq, 0)
        for (key, bad) in [("pass", "short12" as Any), ("pass", String(repeating: "p", count: 64)), ("ssid", ""),
                           ("addr", "0.0.0.0"), ("addr", "nowhere"), ("tls_port", 0), ("port", 70000)] {
            var m = ok
            m[key] = bad
            XCTAssertThrowsError(try DirectAnswer.parse(m, tv: "TV"), "\(key)=\(bad)") { e in
                XCTAssertEqual(e.localizedDescription, "TV answered with a Wi-Fi Direct network this Mac cannot join")
            }
        }
        var noTLS = ok
        noTLS["tls_port"] = nil
        XCTAssertThrowsError(try DirectAnswer.parse(noTLS, tv: "TV"))
    }

    func testRefusalsAreSaidInWords() {
        func said(_ code: String, _ detail: String = "") -> String {
            do {
                _ = try DirectAnswer.parse(["t": "error", "for": "direct", "code": code, "detail": detail], tv: "Living room TV")
                return "no error"
            } catch {
                return error.localizedDescription
            }
        }
        XCTAssertEqual(said("denied"), "Living room TV said no to connecting directly")
        XCTAssertEqual(said("no_p2p", "Nearby devices permission not given"),
                       "Living room TV cannot make a Wi-Fi Direct network: Nearby devices permission not given")
        XCTAssertEqual(said("not_allowed"), "Living room TV does not allow direct connections")
        XCTAssertEqual(said("busy"), "Living room TV is busy with another direct connection; try again in a moment")
        XCTAssertEqual(said("timeout"), "nobody answered on Living room TV in time, or its Wi-Fi Direct network did not come up")
        XCTAssertEqual(said("melted", "x"), "Living room TV refused the direct connection (melted): x")
    }
}

final class DirectWiFiTests: XCTestCase {
    var t: TimeInterval = 100
    let group = DirectAnswer(ssid: "DIRECT-xy-Luch", pass: "k3y-of-twelve", addr: "192.168.49.1", port: 47800, tlsPort: 47802)

    func testItAsksNamingTheNetworkItLeaves() {
        let w = DirectWiFi(wifi: FakeWiFi(on: "Hotel Guest"), now: { self.t })
        XCTAssertEqual(w.question(tv: "Living room TV"),
                       "Connect directly to Living room TV? This Mac leaves 'Hotel Guest' while showing, and comes back after.")
        let unnamed = DirectWiFi(wifi: FakeWiFi(on: nil), now: { self.t })
        XCTAssertEqual(unnamed.question(tv: "Living room TV"),
                       "Connect directly to Living room TV? This Mac leaves its Wi-Fi network while showing, and comes back after.")
    }

    func testItGoesBackOnceShowingHasEnded() throws {
        let fake = FakeWiFi(on: "Hotel Guest")
        let w = DirectWiFi(wifi: fake, now: { self.t })
        try w.join(group, receiverID: "tv-1", name: "Living room TV")
        XCTAssertEqual(fake.calls, ["join DIRECT-xy-Luch k3y-of-twelve"])
        XCTAssertEqual(w.joined?.previous, "Hotel Guest")
        t += 10
        XCTAssertNil(w.tick(busy: false), "not shown yet: it waits")
        t += 10
        XCTAssertNil(w.tick(busy: true))
        t += 10
        XCTAssertNil(w.tick(busy: false), "a short pause, a mirror turning into a cast")
        t += DirectWiFi.endGrace
        XCTAssertEqual(w.tick(busy: false), "left 'DIRECT-xy-Luch' (showing ended): back on 'Hotel Guest'")
        XCTAssertEqual(fake.calls.suffix(2), ["leave", "rejoin Hotel Guest"])
        XCTAssertNil(w.joined)
        XCTAssertNil(w.tick(busy: false))
    }

    func testNothingShownGoesBackWhenTheGroupWouldGo() throws {
        let fake = FakeWiFi(on: "Hotel Guest")
        let w = DirectWiFi(wifi: fake, now: { self.t })
        try w.join(group, receiverID: "tv-1", name: "Living room TV")
        t += DirectWiFi.idleLimit - 1
        XCTAssertNil(w.tick(busy: false))
        t += 1
        XCTAssertEqual(w.tick(busy: false), "left 'DIRECT-xy-Luch' (nothing was shown): back on 'Hotel Guest'")
    }

    func testOffLeavesAtOnceAndSaysWhenItCannotGoBack() throws {
        let fake = FakeWiFi(on: "Hotel Guest")
        fake.rejoinFails = true
        let w = DirectWiFi(wifi: fake, now: { self.t })
        try w.join(group, receiverID: "tv-1", name: "Living room TV")
        XCTAssertEqual(try w.off(), "left 'DIRECT-xy-Luch' (asked to), but could not go back to 'Hotel Guest': choose it in the Wi-Fi menu")
        XCTAssertThrowsError(try w.off()) { XCTAssertEqual($0.localizedDescription, "not connected directly") }

        // The network it left was not known by name: macOS's auto-join picks.
        let unnamed = FakeWiFi(on: nil)
        let u = DirectWiFi(wifi: unnamed, now: { self.t })
        try u.join(group, receiverID: "tv-1", name: "Living room TV")
        XCTAssertEqual(try u.off(), "left 'DIRECT-xy-Luch' (asked to): macOS picks the Wi-Fi network")
        XCTAssertEqual(unnamed.calls, ["join DIRECT-xy-Luch k3y-of-twelve", "leave", "rejoin -"])
    }

    func testAFailedJoinGoesBackAndASecondKeepsTheFirstNetwork() throws {
        let fake = FakeWiFi(on: "Hotel Guest")
        fake.joinFails = "the network is not in range"
        let w = DirectWiFi(wifi: fake, now: { self.t })
        XCTAssertThrowsError(try w.join(group, receiverID: "tv-1", name: "TV")) {
            XCTAssertEqual($0.localizedDescription, "could not join 'DIRECT-xy-Luch': the network is not in range")
        }
        XCTAssertEqual(fake.calls, ["join DIRECT-xy-Luch k3y-of-twelve", "rejoin Hotel Guest"])
        XCTAssertNil(w.joined)

        fake.joinFails = nil
        try w.join(group, receiverID: "tv-1", name: "TV")
        var other = group
        other.ssid = "DIRECT-zz-Luch"
        try w.join(other, receiverID: "tv-2", name: "Kitchen TV")
        XCTAssertEqual(w.joined?.previous, "Hotel Guest", "not the first direct network")
        XCTAssertEqual(w.question(tv: "TV"), "Connect directly to TV? This Mac leaves 'Hotel Guest' while showing, and comes back after.")
    }
}

/// The whole of it against the Swift receiver: heard over the air, asked on screen, paired over the channel,
/// asked for its group, joined, listed; and back.
final class DirectFlowTests: XCTestCase {
    var tv: FakeTLSReceiver!
    var connector: Connector!
    var finder: Finder!
    var wifi: FakeWiFi!
    var channels: FakeChannels!
    var direct: Direct!
    var t: TimeInterval = 0
    var mine: SenderCertificate!
    /// The receiver's whole id; its card has the first 8 hex digits.
    let tvID = "0b6f1c2a-5d1e-4c55-9a1d-1c2a3b4c5d6e"
    let card = Card(kind: Card.receiver, version: 0, id: "0b6f1c2a", addr: nil, psm: 0x80)

    override func setUpWithError() throws {
        tv = try FakeTLSReceiver(dir: try tempDir())
        tv.id = tvID
        tv.directAnswer = ["t": "direct", "ssid": "DIRECT-xy-Luch", "pass": "k3y-of-twelve", "freq": 5180,
                           "addr": "192.168.49.1", "port": 47800, "tls_port": 47802]
        let dir = try tempDir()
        let mine = try SenderCertificate.load(dir: dir)
        self.mine = mine
        connector = Connector(dir: dir, certificate: { mine })
        let me = ProbeIdentity(id: "mac-1", name: "Mac")
        finder = Finder(me: me, known: KnownReceivers(file: dir.appendingPathComponent("known.json")), clock: TestClock()) { [] }
        wifi = FakeWiFi(on: "Hotel Guest")
        channels = FakeChannels(tlsPort: tv.port)
        direct = Direct(finder: finder, me: me, connector: connector, wifi: wifi, channels: channels, now: { self.t })
        direct.watchEvery = nil
        direct.listen = { [card] _ in [Heard(card: card, rssi: -48, device: UUID())] }
        direct.reach = { a, id, name, _ in
            (Receiver(id: id, name: name, model: "", address: a.addr, interfaceName: "en0", interfaceIndex: 4,
                      port: a.port, service: "", tlsPort: a.tlsPort), [:])
        }
    }

    func testSayingNoChangesNothing() {
        var asked: String?
        direct.confirm = { asked = $0; return false }
        XCTAssertThrowsError(try direct.connect(nil, confirmed: false)) {
            XCTAssertEqual($0.localizedDescription, "not connected directly: this Mac stays on its network")
        }
        XCTAssertEqual(asked, "Connect directly to receiver 0b6f1c2a? This Mac leaves 'Hotel Guest' while showing, and comes back after.")
        XCTAssertEqual(wifi.calls, ["allow"], "the Wi-Fi is only asked its name")
        XCTAssertEqual(channels.opened, 0, "nor is the TV asked")
        XCTAssertEqual(direct.status()["state"] as? String, "idle")
        let nearby = direct.status()["nearby"] as? [[String: Any]]
        XCTAssertEqual(nearby?.first?["id"] as? String, "0b6f1c2a")
        XCTAssertEqual(nearby?.first?["listed"] as? Bool, false)
    }

    func testPairedOverBluetoothThenJoinedListedAndBack() throws {
        direct.confirm = { _ in true }
        // The code typed on this Mac once the TV shows it (the app's window, or luchctl pair).
        let typed = expectation(description: "the code was typed")
        Thread.detachNewThread { [direct, connector] in
            let until = Date().addingTimeInterval(10)
            while Date() < until {
                if let s = direct?.status(), s["state"] as? String == "pairing", let id = s["id"] as? String,
                   connector?.pairing(id)["step"] as? String == "code" {
                    XCTAssertEqual(id, "0b6f1c2a", "known only by its card yet")
                    _ = try? connector?.pair(id, code: "123456")
                    typed.fulfill()
                    return
                }
                usleep(50000)
            }
        }
        let said = try direct.connect("0b6f1c2a", confirmed: false)
        wait(for: [typed], timeout: 1)
        XCTAssertEqual(said, "joined 'DIRECT-xy-Luch': Living room TV is at 192.168.49.1; mirror or cast to it as usual. "
            + "This Mac goes back to 'Hotel Guest' after")
        XCTAssertEqual(channels.opened, 2, "one channel for pairing, one for the request")
        XCTAssertEqual(wifi.calls, ["allow", "join DIRECT-xy-Luch k3y-of-twelve"])
        // The pin moved to the id the welcome gave.
        XCTAssertTrue(connector.isPaired(tvID))
        XCTAssertFalse(connector.isPaired("0b6f1c2a"))
        let hello = tv.messages.last { $0["t"] as? String == "hello" }
        XCTAssertEqual(hello?["via"] as? String, "ble")
        XCTAssertEqual(hello?["want"] as? [String], ["direct"])
        XCTAssertTrue(tv.messages.contains { $0["t"] as? String == "direct" })
        XCTAssertEqual(finder.list().map(\.id), [tvID])
        XCTAssertEqual(finder.discovered()[tvID]?["via"] as? [String], ["direct"])
        let s = direct.status()
        XCTAssertEqual(s["state"] as? String, "joined")
        XCTAssertEqual(s["ssid"] as? String, "DIRECT-xy-Luch")
        XCTAssertEqual(s["previous"] as? String, "Hotel Guest")

        // Shown, then no longer: back on the hotel's Wi-Fi, the receiver no longer listed.
        var shown = true
        direct.isBusy = { _ in shown }
        var told: String?
        direct.onLeft = { told = $0 }
        t += 5
        XCTAssertTrue(direct.check())
        shown = false
        t += DirectWiFi.endGrace
        XCTAssertFalse(direct.check())
        XCTAssertEqual(told, "left 'DIRECT-xy-Luch' (showing ended): back on 'Hotel Guest'")
        XCTAssertEqual(wifi.calls.suffix(2), ["leave", "rejoin Hotel Guest"])
        XCTAssertTrue(finder.list().isEmpty)
        XCTAssertEqual(direct.status()["state"] as? String, "idle")

        // Paired now: the next time goes straight to the request, under the whole id.
        let again = try direct.connect(nil, confirmed: true)
        XCTAssertTrue(again.hasPrefix("joined 'DIRECT-xy-Luch'"), again)
        XCTAssertEqual(channels.opened, 3)
        XCTAssertEqual(try direct.off(), "left 'DIRECT-xy-Luch' (asked to): back on 'Hotel Guest'")
    }

    func testARefusalOnTheTVIsSaidAndTheWiFiStays() throws {
        tv.trust(mine.fp)
        try Pins.pin("0b6f1c2a", fp: tv.certificate.fp, name: "Living room TV", dir: connector.dir)
        tv.directAnswer = ["t": "error", "for": "direct", "code": "denied"]
        XCTAssertThrowsError(try direct.connect(nil, confirmed: true)) {
            XCTAssertEqual($0.localizedDescription, "Living room TV said no to connecting directly")
        }
        XCTAssertEqual(direct.status()["state"] as? String, "failed")
        XCTAssertEqual(wifi.calls, ["allow"])
    }
}
