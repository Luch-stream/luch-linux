// SPDX-License-Identifier: GPL-3.0-or-later
import XCTest
import LuchProto
@testable import LuchNet

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
}
