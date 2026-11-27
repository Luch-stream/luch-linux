// SPDX-License-Identifier: GPL-3.0-or-later
import CryptoKit
import Darwin
import XCTest
import LuchEngine
@testable import LuchNet
@testable import LuchProto

/// Pairing end to end over TLS on loopback, against a receiver made of Swift (FakeTLSReceiver): what luchd's
/// tests do against a TV made of Rust.
final class PairingFlowTests: XCTestCase {
    var dir: URL!
    var tv: FakeTLSReceiver!
    var me: SenderCertificate!
    var connector: Connector!

    override func setUpWithError() throws {
        dir = try tempDir()
        tv = try FakeTLSReceiver(dir: try tempDir())
        let mine = try SenderCertificate.load(dir: dir)
        me = mine
        connector = Connector(dir: dir, certificate: { mine })
    }

    var receiver: Receiver {
        Receiver(id: "tv-1", name: "Living room TV", model: "Acme Box", address: "127.0.0.1", interfaceName: "lo0",
                 interfaceIndex: 0, port: 1, service: "tv", tlsPort: tv.port)
    }

    let hello: [String: Any] = ["t": "hello", "v": 0, "id": "mac-1", "name": "Mac", "app": "test", "want": ["mirror", "reports"]]

    /// Waits for pairing to reach `step` (or fail).
    func waitFor(_ step: String) -> [String: Any] {
        let until = Date().addingTimeInterval(5)
        while Date() < until {
            let st = connector.pairing("tv-1")
            if st["step"] as? String == step || st["step"] as? String == "failed" { return st }
            usleep(50_000)
        }
        return connector.pairing("tv-1")
    }

    func testTheCodeIsAskedForOnceThenThePinLetsIn() throws {
        XCTAssertThrowsError(try connector.open(receiver, hello: hello)) { e in
            guard case let ConnectError.pairingNeeded(id, _, allowFirst) = e else { return XCTFail("\(e)") }
            XCTAssertEqual(id, "tv-1")
            XCTAssertFalse(allowFirst)
            XCTAssertEqual(e.localizedDescription,
                           "Living room TV asks for pairing: enter the code it shows (`luchctl pair tv-1`)")
        }
        XCTAssertEqual(tv.messages.first?["v"] as? Int, 1)
        XCTAssertEqual(tv.messages.first?["pair"] as? Bool, true, "no pin yet: the hello asks to pair")
        XCTAssertEqual(connector.pairing("tv-1")["step"] as? String, "code")
        XCTAssertThrowsError(try connector.open(receiver, hello: hello), "a code is waiting: nothing else connects")

        XCTAssertEqual(try connector.pair("tv-1", code: "123 456"), "Living room TV")
        XCTAssertEqual(Pins.fingerprint("tv-1", dir: dir), tv.certificate.fp, "the receiver's certificate is pinned")
        XCTAssertTrue(tv.isPaired(me.fp), "and the receiver pinned this Mac's")
        XCTAssertEqual(waitFor("paired")["step"] as? String, "paired")
        XCTAssertEqual(connector.paired().first?["name"] as? String, "Living room TV")

        let c = try connector.open(receiver, hello: hello)
        XCTAssertTrue(c.secure)
        XCTAssertEqual(c.welcome["v"] as? Int, 1)
        XCTAssertNil(tv.messages.last(where: { $0["t"] as? String == "hello" })?["pair"], "pinned: no pairing asked for")
        c.channel.bye()
    }

    func testAWrongCodeIsRefusedAndNothingPinned() throws {
        XCTAssertThrowsError(try connector.open(receiver, hello: hello))
        XCTAssertThrowsError(try connector.pair("tv-1", code: "000000")) { e in
            XCTAssertEqual(e.localizedDescription, "wrong code: try again, Living room TV shows a new one")
        }
        XCTAssertNil(Pins.fingerprint("tv-1", dir: dir))
        XCTAssertEqual(waitFor("failed")["step"] as? String, "failed")
        XCTAssertThrowsError(try connector.pair("tv-1", code: "123456")) { e in
            XCTAssertTrue(e.localizedDescription.contains("connect to it first"))
        }
    }

    func testTheTVMayAskItsViewersFirst() throws {
        tv.askFirst = true
        XCTAssertThrowsError(try connector.open(receiver, hello: hello)) { e in
            guard case let ConnectError.pairingNeeded(_, _, allowFirst) = e else { return XCTFail("\(e)") }
            XCTAssertTrue(allowFirst)
            XCTAssertTrue(e.localizedDescription.contains("allow it on the TV, then enter the code it shows"))
        }
        XCTAssertThrowsError(try connector.pair("tv-1", code: "123456")) { e in
            XCTAssertEqual(e.localizedDescription, "no code on the TV yet: allow the connection there first")
        }
        XCTAssertEqual(waitFor("code")["step"] as? String, "code")
        try connector.pair("tv-1", code: "123456")
        XCTAssertNotNil(Pins.fingerprint("tv-1", dir: dir))
    }

    func testAnotherCertificateUnderAPairedIDIsRefused() throws {
        try Pins.pin("tv-1", fp: [UInt8](repeating: 1, count: 32), name: "Living room TV", dir: dir)
        XCTAssertThrowsError(try connector.open(receiver, hello: hello)) { e in
            XCTAssertTrue(e.localizedDescription.contains("not the certificate this receiver was paired with"))
            XCTAssertTrue(e.localizedDescription.contains("`luchctl forget tv-1`"))
        }
    }

    func testAPairedReceiverIsNeverReachedWithoutTLS() throws {
        try Pins.pin("tv-1", fp: tv.certificate.fp, name: "Living room TV", dir: dir)
        var r = receiver
        r.tlsPort = 0
        XCTAssertThrowsError(try connector.open(r, hello: hello)) { e in
            XCTAssertTrue(e.localizedDescription.contains("was paired but now offers no encryption"))
        }
    }

    /// Reports come only over TLS, are kept, answered with reports_taken, and the new one is told about.
    func testReportsAreKeptAndTaken() throws {
        let id = "3f0c2a8e-9d1b-4c7e-8a55-0b6f2d9e1c47"
        tv.reports = [["id": id, "kind": "crash", "time": 1, "count": 1, "what": "IllegalStateException"]]
        XCTAssertThrowsError(try connector.open(receiver, hello: hello), "the code first")
        try connector.pair("tv-1", code: "123456")
        let told = expectation(description: "a new report is told about")
        Reports.onNew = { r, name in
            XCTAssertEqual(r["receiver_model"] as? String, "Acme Box")
            XCTAssertEqual(name, "Living room TV")
            told.fulfill()
        }
        defer { Reports.onNew = nil }
        let c = try connector.open(receiver, hello: hello)
        wait(for: [told], timeout: 5)
        let until = Date().addingTimeInterval(3)
        while Date() < until, !tv.messages.contains(where: { $0["t"] as? String == "reports_taken" }) { usleep(50_000) }
        XCTAssertEqual(tv.messages.last(where: { $0["t"] as? String == "reports_taken" })?["ids"] as? [String], [id])
        XCTAssertEqual(Reports.list(base: dir).count, 1)
        c.channel.bye()
    }

    /// A session with a paired receiver: its media key from `ready`, every datagram sealed (sound only here, the
    /// path that needs no screen; video goes through the same sender).
    func testASessionWithAPairedReceiverIsSealed() throws {
        let udp = socket(AF_INET, SOCK_DGRAM, 0)
        defer { close(udp) }
        var a = sockaddr_in()
        a.sin_len = UInt8(MemoryLayout<sockaddr_in>.size)
        a.sin_family = sa_family_t(AF_INET)
        a.sin_addr.s_addr = inet_addr("127.0.0.1")
        var len = socklen_t(MemoryLayout<sockaddr_in>.size)
        withUnsafeMutablePointer(to: &a) { p in
            p.withMemoryRebound(to: sockaddr.self, capacity: 1) {
                _ = Darwin.bind(udp, $0, len)
                _ = getsockname(udp, $0, &len)
            }
        }
        var timeout = timeval(tv_sec: 3, tv_usec: 0)
        setsockopt(udp, SOL_SOCKET, SO_RCVTIMEO, &timeout, socklen_t(MemoryLayout<timeval>.size))
        tv.mediaPort = UInt16(bigEndian: a.sin_port)
        tv.trust(me.fp)
        try Pins.pin("tv-1", fp: tv.certificate.fp, name: "Living room TV", dir: dir)
        let saved = Connector.shared
        Connector.shared = connector
        defer { Connector.shared = saved }

        var options = MirrorOptions()
        options.soundOnly = true
        options.synthetic = true
        let engine = Engine(options: options)
        try engine.start(receiver: receiver)
        defer { engine.stop(reason: "test") }

        var b = [UInt8](repeating: 0, count: 2048)
        let n = recv(udp, &b, b.count, 0)
        XCTAssertGreaterThan(n, Proto.headerSize)
        let h = Array(b[0..<Proto.headerSize])
        XCTAssertEqual(h[1], PacketFlags.config.rawValue)
        XCTAssertEqual(Int(h.be32(16)), 19 + MediaSeal.tag, "the length counts the tag")
        var aad = h
        aad[2] = 0
        aad[3] = 0
        var nonce = [UInt8](repeating: 0, count: 12)
        nonce[0] = MediaKind.audio.rawValue
        nonce[4..<8] = h[8..<12]
        let body = Array(b[Proto.headerSize..<n])
        let box = try AES.GCM.SealedBox(nonce: AES.GCM.Nonce(data: nonce), ciphertext: body.dropLast(16), tag: body.suffix(16))
        let plain = try AES.GCM.open(box, using: SymmetricKey(data: tv.mediaKey), authenticating: aad)
        XCTAssertEqual(Array(plain.prefix(8)), Array("OpusHead".utf8))
    }
}
