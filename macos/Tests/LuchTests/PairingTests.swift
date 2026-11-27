// SPDX-License-Identifier: GPL-3.0-or-later
import Security
import XCTest
import LuchLocal
@testable import LuchNet

/// The pieces of pairing (PROTOCOL.md 3a), with luchd's pair.rs tests and vectors.
final class PairingTests: XCTestCase {
    /// The same bytes as the receiver's PairingTest.commitmentIsTheSameOnEveryImplementation and luchd's
    /// commitment_matches_the_receiver.
    func testCommitmentMatchesTheReceiver() {
        let c = Pairing.commitment(role: Pairing.sender, round: 0, digit: UInt8(ascii: "4"), nonce: Array(0..<16),
                                   sender: [UInt8](repeating: 0x11, count: 32),
                                   receiver: [UInt8](repeating: 0x22, count: 32))
        XCTAssertEqual(hex(c), "48826a53f4d157eb5596913e25946f39730c10aeafabc935e98c7ac732544373")
    }

    func testCodesAreSixDigitsSpacesAllowed() throws {
        XCTAssertEqual(try Pairing.cleanCode("123 456"), Array("123456".utf8))
        XCTAssertEqual(try Pairing.cleanCode("123-456"), Array("123456".utf8))
        XCTAssertThrowsError(try Pairing.cleanCode("12345"))
        XCTAssertThrowsError(try Pairing.cleanCode("12345a"))
    }

    func testRefusalsReadWell() {
        let tv = "Living room TV"
        XCTAssertEqual(Pairing.explain(.refused("banned", 3500), tv: tv),
                       "Living room TV was refused too often from here and ignores this computer for 59 min")
        XCTAssertEqual(Pairing.explain(.refused("busy", 30), tv: tv),
                       "Living room TV is already asking about another device; try again in 30 s")
        XCTAssertTrue(Pairing.explain(.badReceiver, tv: tv).contains("in between"))
    }

    func testPinsAreKeptPerReceiverAndForgotten() throws {
        let dir = try tempDir()
        XCTAssertNil(Pins.fingerprint("tv", dir: dir))
        try Pins.pin("tv", fp: [UInt8](repeating: 7, count: 32), name: "Living room TV", dir: dir)
        XCTAssertEqual(Pins.fingerprint("tv", dir: dir), [UInt8](repeating: 7, count: 32))
        XCTAssertEqual(Pins.all(dir: dir)["tv"]?["name"] as? String, "Living room TV")
        XCTAssertTrue(try Pins.forget("tv", dir: dir))
        XCTAssertFalse(try Pins.forget("tv", dir: dir))
        XCTAssertNil(Pins.fingerprint("tv", dir: dir))
    }

    /// luchd's paired.json reads here, and the other way round: one file format for both senders.
    func testPinsReadLuchdsFile() throws {
        let dir = try tempDir()
        let fp = String(repeating: "ab", count: 32)
        try Data(#"{"0b6f-tv": {"fp": "\#(fp)", "name": "Living room TV", "paired": 1700000000}}"#.utf8)
            .write(to: dir.appendingPathComponent("paired.json"))
        XCTAssertEqual(Pins.fingerprint("0b6f-tv", dir: dir), [UInt8](repeating: 0xab, count: 32))
    }

    func testIdentityIsKeptWithAPrivateKey() throws {
        let dir = try tempDir()
        let a = try SenderCertificate.load(dir: dir)
        let b = try SenderCertificate.load(dir: dir)
        XCTAssertEqual(a.fp, b.fp)
        XCTAssertEqual(a.fp, fingerprint(a.der))
        let mode = try FileManager.default.attributesOfItem(atPath: dir.appendingPathComponent("sender-key.pem").path)[.posixPermissions] as? Int
        XCTAssertEqual(mode, 0o600)
        let cert = try XCTUnwrap(SecCertificateCreateWithData(nil, Data(a.der) as CFData))
        XCTAssertEqual(SecCertificateCopySubjectSummary(cert) as String?, "Luch sender")
        // Its own key signs: the certificate's signature checks out against its public key.
        let key = try XCTUnwrap(SecCertificateCopyKey(cert))
        var trust: SecTrust?
        XCTAssertEqual(SecTrustCreateWithCertificates(cert, SecPolicyCreateBasicX509(), &trust), errSecSuccess)
        SecTrustSetAnchorCertificates(trust!, [cert] as CFArray)
        XCTAssertTrue(SecTrustEvaluateWithError(trust!, nil), "self-signed, valid until 2055")
        XCTAssertEqual(SecKeyGetBlockSize(key), 32)
    }

    /// luchctl's a_refusal_that_asks_for_pairing_names_the_receiver: the words the app answers with are luchd's.
    func testARefusalThatAsksForPairingNamesTheReceiver() {
        let msg = ConnectError.pairingNeeded(id: "0b6f-tv", name: "Living room TV", allowFirst: false).localizedDescription
        XCTAssertEqual(msg, "Living room TV asks for pairing: enter the code it shows (`luchctl pair 0b6f-tv`)")
        XCTAssertEqual(pairingID(msg), "0b6f-tv")
        XCTAssertNil(pairingID("Living room TV refused the connection"))
        XCTAssertTrue(isPairCode("123456") && !isPairCode("Living") && !isPairCode("12345"))
    }

    func testDERLengthsAndIntegers() {
        XCTAssertEqual(DER.tlv(0x04, [UInt8](repeating: 0, count: 200)).prefix(3), [0x04, 0x81, 200])
        XCTAssertEqual(DER.tlv(0x04, [UInt8](repeating: 0, count: 300)).prefix(4), [0x04, 0x82, 0x01, 0x2c])
        XCTAssertEqual(DER.int([0x80]), [0x02, 0x02, 0x00, 0x80])
        XCTAssertEqual(DER.int([0x00, 0x00, 0x7f]), [0x02, 0x01, 0x7f])
        XCTAssertEqual(DER.oid("1.2.840.10045.4.3.2"), [0x06, 0x08, 0x2a, 0x86, 0x48, 0xce, 0x3d, 0x04, 0x03, 0x02])
    }
}

func tempDir() throws -> URL {
    let d = FileManager.default.temporaryDirectory.appendingPathComponent("luch-test-\(UUID().uuidString)")
    try FileManager.default.createDirectory(at: d, withIntermediateDirectories: true)
    return d
}
