// Known receivers and waking them, with luchd's wake.rs vectors; Bluetooth's manufacturer data as CoreBluetooth gives
// it; the receiver's QR code read by Vision; the wake relay against a receiver on loopback.
// SPDX-License-Identifier: GPL-3.0-or-later
import CoreImage
import Foundation
@testable import LuchFind
import LuchLocal
@testable import LuchNet
import XCTest

final class KnownTests: XCTestCase {
    func testMacsParseAndPrint() {
        XCTAssertEqual(parseMac("AA-bb-01-02-03-04").map(formatMac), "aa:bb:01:02:03:04")
        XCTAssertNil(parseMac("00:00:00:00:00:00"))
        XCTAssertNil(parseMac("aa:bb:cc"))
    }

    func testAMagicPacketIsSixFFAndSixteenMacs() {
        let p = magicPacket([1, 2, 3, 4, 5, 6])
        XCTAssertEqual(p.count, 102)
        XCTAssertEqual(Array(p[..<6]), [UInt8](repeating: 0xff, count: 6))
        XCTAssertEqual(Array(p[96...]), [1, 2, 3, 4, 5, 6])
    }

    /// The packet goes out on the LAN that held the receiver, to its own broadcast as well, never into the VPN.
    func testWakeOnLanIsBroadcastOnTheRightInterface() {
        let up = UInt32(IFF_UP | IFF_RUNNING)
        let lan = NetInterface(name: "en0", index: 4, addr: 0xC0A8_010A, prefix: 24, flags: up)
        let vpn = NetInterface(name: "utun3", index: 9, addr: 0x0A08_0002, prefix: 8, flags: up | UInt32(IFF_POINTOPOINT))
        let t = wolTargets(lastAddress: "192.168.1.20", ifaces: [vpn, lan])!
        XCTAssertEqual(t.iface.name, "en0")
        XCTAssertEqual(t.to, [UInt32.max, 0xC0A8_01FF])
        XCTAssertEqual(wolTargets(lastAddress: "10.1.2.3", ifaces: [vpn, lan])?.iface.name, "en0")
        XCTAssertNil(wolTargets(lastAddress: nil, ifaces: [vpn]))
    }

    func testSettingsAreCheckedAndKeptPerReceiver() throws {
        var all: [String: [String: Any]] = [:]
        let id = "3f0c2a8e-9d1b-4c7e-8a55-0b6f2d9e1c47"
        XCTAssertThrowsError(try KnownReceivers.set(&all, id: id, kind: "mac", value: "aa:bb:cc:dd:ee:ff"))
        KnownReceivers.merge(&all, id: id, name: "Living room TV", model: "XGIMI WK03K", address: "192.168.1.20", mac: nil, now: 1)
        XCTAssertNotNil(KnownReceivers.hint(all[id]!))
        XCTAssertEqual(KnownReceivers.methods(all[id]!), [])
        XCTAssertThrowsError(try KnownReceivers.set(&all, id: id, kind: "ble", value: "zz"))
        XCTAssertThrowsError(try KnownReceivers.set(&all, id: id, kind: "ble", value: "e71"), "odd hex")
        try KnownReceivers.set(&all, id: id, kind: "ble", value: "E7129730 35F278FF FFFF3043 524B544D")
        try KnownReceivers.set(&all, id: id, kind: "mac", value: "AA-BB-CC-DD-EE-FF")
        XCTAssertEqual(all[id]?["ble"] as? String, "e712973035f278ffffff3043524b544d")
        XCTAssertEqual(all[id]?["mac"] as? String, "aa:bb:cc:dd:ee:ff")
        XCTAssertEqual(KnownReceivers.methods(all[id]!), ["wol", "ble"])
        XCTAssertNil(KnownReceivers.hint(all[id]!))
        // A later connection updates the address but keeps what was learned.
        KnownReceivers.merge(&all, id: id, name: "Living room TV", model: "XGIMI WK03K", address: "192.168.1.30", mac: nil, now: 2)
        XCTAssertEqual(all[id]?["address"] as? String, "192.168.1.30")
        XCTAssertEqual(KnownReceivers.methods(all[id]!), ["wol", "ble"])
        try KnownReceivers.set(&all, id: id, kind: "mac", value: "")
        XCTAssertEqual(KnownReceivers.methods(all[id]!), ["ble"])
        XCTAssertThrowsError(try KnownReceivers.set(&all, id: id, kind: "ir", value: "1"))
    }

    func testAReinstalledReceiverReplacesItsOldEntry() throws {
        var all: [String: [String: Any]] = [:]
        let mac = "aa:bb:cc:dd:ee:ff"
        KnownReceivers.merge(&all, id: "old", name: "Living room TV", model: "X", address: "192.168.1.5", mac: mac, now: 1)
        try KnownReceivers.set(&all, id: "old", kind: "ble", value: "008a433321129cffffff3043524b544d")
        KnownReceivers.merge(&all, id: "dev", name: "Living room TV (dev)", model: "X", address: "192.168.1.5", mac: mac, now: 2)
        KnownReceivers.merge(&all, id: "other", name: "Bedroom TV", model: "X", address: "192.168.1.6", mac: "11:22:33:44:55:66", now: 3)
        KnownReceivers.merge(&all, id: "new", name: "Living room TV", model: "X", address: "192.168.1.5", mac: mac, now: 4)
        XCTAssertEqual(all.keys.sorted(), ["dev", "new", "other"], "the dev build is another app and stays")
        XCTAssertEqual(all["new"]?["ble"] as? String, "008a433321129cffffff3043524b544d")
        // Without a MAC nothing can be told apart, so nothing is dropped.
        KnownReceivers.merge(&all, id: "newer", name: "Living room TV", model: "X", address: "192.168.1.5", mac: nil, now: 5)
        XCTAssertNotNil(all["new"])
    }

    func testKnownReceiversSurviveARestartAndAreListed() throws {
        let f = FileManager.default.temporaryDirectory.appendingPathComponent("luch-known-\(getpid()).json")
        defer { try? FileManager.default.removeItem(at: f) }
        KnownReceivers(file: f).remember(id: "a", name: "TV", model: "", address: "192.168.1.5", mac: "aa:bb:cc:dd:ee:ff")
        XCTAssertEqual(KnownReceivers(file: f).load()["a"]?["mac"] as? String, "aa:bb:cc:dd:ee:ff")
        let finder = Finder(me: ProbeIdentity(id: "s", name: "Test Mac"), known: KnownReceivers(file: f)) { [] }
        let rows = finder.knownRows()
        XCTAssertEqual(rows.count, 1)
        XCTAssertEqual(rows[0]["online"] as? Bool, false, "not on the LAN now")
        XCTAssertEqual(rows[0]["wake"] as? [String], ["wol"])
        XCTAssertEqual(FindText.known(rows).first?.hasSuffix("off      wake: wol"), true, "\(FindText.known(rows))")
        XCTAssertEqual(try FindText.pickKnown(rows, "tv")["id"] as? String, "a")
        XCTAssertThrowsError(try FindText.pickKnown(rows, "kitchen"))
        // Asleep with no way known yet: said plainly, nothing is sent.
        XCTAssertThrowsError(try finder.wake("nobody"))
    }
}

final class BluetoothTests: XCTestCase {
    func testCardsAndRemotesAreReadFromManufacturerData() {
        let card = Card(kind: Card.receiver, version: 0, id: "3f0c2a8e", addr: (tv192, 47800))
        // CoreBluetooth gives the company id first, little-endian.
        XCTAssertEqual(BLE.card(Data([0xff, 0xff] + card.encode())), card)
        XCTAssertNil(BLE.card(Data([0xff, 0xff, 1, 2, 3, 4, 5, 6, 7, 8])), "someone else's test data")
        XCTAssertNil(BLE.card(Data([0x4c, 0x00] + card.encode())), "another company")
        XCTAssertNil(BLE.card(Data([0xff, 0xff] + Card.looking("dddddddd").encode())), "another sender looking")
        let remote = parseHex("7e8089b6a22f1cffffff3043524b544d")!
        XCTAssertEqual(BLE.remote(Data([0x46, 0x00] + remote)), remote)
        XCTAssertNil(BLE.remote(Data([0x4c, 0x00] + remote)))
        XCTAssertNil(BLE.remote(Data([0x46, 0x00, 0x7e, 0x80, 0x89])), "too short for a power key")
        XCTAssertNil(BLE.remote(nil))
    }

    func testTheNearestReceiverComesFirstOnce() {
        func h(_ id: String, _ rssi: Int?) -> Heard {
            Heard(card: Card(kind: Card.receiver, id: id, addr: (tv192, 47800)), rssi: rssi, device: UUID())
        }
        let got = nearestFirst([h("aaaaaaaa", -80), h("bbbbbbbb", -40), h("cccccccc", nil), h("aaaaaaaa", -50)])
        XCTAssertEqual(got.map(\.card.id), ["bbbbbbbb", "aaaaaaaa", "cccccccc"])
        XCTAssertEqual(got.map(\.rssi), [-40, -50, nil])
    }

    /// Another Luch device that can advertise sends the remote's signal for us; one that cannot is passed over.
    func testTheRemotesSignalIsRelayedByAReceiverThatCan() throws {
        let cannot = LoopbackReceiver(id: "aaaaaaaa-0000-4000-8000-000000000001", name: "Bedroom TV")
        let can = LoopbackReceiver(id: "bbbbbbbb-0000-4000-8000-000000000002", name: "Kitchen phone", modes: ["ble_wake"])
        func relay(_ r: LoopbackReceiver, _ name: String) -> Receiver {
            Receiver(id: "", name: name, model: "", address: "127.0.0.1", interfaceName: "lo0", interfaceIndex: 0,
                     port: r.port, service: "")
        }
        let relays = [relay(cannot, "Bedroom TV"), relay(can, "Kitchen phone")]
        let adv = parseHex("7e8089b6a22f1cffffff3043524b544d")!
        let me = ProbeIdentity(id: "s", name: "Test Mac")
        XCTAssertEqual(try WakeRelay.ask(relays, data: adv, holdMs: 100, me: me), "Kitchen phone")
        let asked = can.requests.first { $0["t"] as? String == "ble_wake" }
        XCTAssertEqual(asked?["data"] as? String, "7e8089b6a22f1cffffff3043524b544d")
        XCTAssertEqual(asked?["company"] as? Int, 0x46)
        XCTAssertNil(cannot.requests.first { $0["t"] as? String == "ble_wake" })
        XCTAssertThrowsError(try WakeRelay.ask([relays[0]], data: adv, holdMs: 100, me: me)) {
            XCTAssertTrue($0.localizedDescription.contains("no \"ble_wake\""), $0.localizedDescription)
        }
        XCTAssertEqual(WakeRelay.refusal(["t": "error", "for": "ble_wake", "code": "no_ble"], from: "TV"),
                       "TV has no Bluetooth that can advertise")
    }
}

final class QRTests: XCTestCase {
    let link = "luch://connect?host=192.168.1.20&port=47800&id=3f0c2a8e&t=0badf00d"

    /// A code drawn by Core Image, in the middle of a bigger grey picture: dark on light, or as the TV draws it,
    /// light on dark.
    func picture(_ text: String, tvStyle: Bool) -> CIImage {
        let f = CIFilter(name: "CIQRCodeGenerator")!
        f.setValue(Data(text.utf8), forKey: "inputMessage")
        f.setValue("H", forKey: "inputCorrectionLevel")
        var code = f.outputImage!.transformed(by: CGAffineTransform(scaleX: 6, y: 6))
        if tvStyle { code = code.applyingFilter("CIColorInvert") }
        let room = CIImage(color: CIColor(red: 0.4, green: 0.4, blue: 0.4)).cropped(to: CGRect(x: 0, y: 0, width: 640, height: 480))
        return code.transformed(by: CGAffineTransform(translationX: 200, y: 100)).composited(over: room)
    }

    func testAPrintedCodeAndATVsLightOnDarkCodeAreRead() {
        for tv in [false, true] {
            let got = QR.cards(picture(link, tvStyle: tv))
            XCTAssertEqual(got.count, 1, "tv style \(tv)")
            XCTAssertEqual(got.first?.addr?.ip, tv192)
            XCTAssertEqual(got.first?.token, "0badf00d")
            XCTAssertEqual(got.first?.id, "3f0c2a8e")
        }
    }

    func testOtherCodesAreNotReceivers() {
        XCTAssertEqual(QR.texts(picture("https://example.org/", tvStyle: false)), ["https://example.org/"])
        XCTAssertTrue(QR.cards(picture("https://example.org/", tvStyle: false)).isEmpty)
        XCTAssertTrue(QR.cards(CIImage(color: .gray).cropped(to: CGRect(x: 0, y: 0, width: 64, height: 64))).isEmpty)
    }

    /// A code made by the receiver's own encoder (QrCode.java, drawn by its QrCodeTest into luchd's fixture) and
    /// read here: both ends of the QR path, written apart.
    func testTheReceiversOwnCodeIsRead() throws {
        let pgm = URL(fileURLWithPath: #filePath).deletingLastPathComponent().appendingPathComponent("../../../luchd/tests/receiver-qr.pgm")
        let (w, h, luma) = try readPGM(Data(contentsOf: pgm))
        let image = CIImage(bitmapData: luma, bytesPerRow: w, size: CGSize(width: w, height: h), format: .L8,
                            colorSpace: CGColorSpaceCreateDeviceGray())
        let got = QR.cards(image)
        XCTAssertEqual(got.count, 1)
        // The receiver leaves the id out (its token says which TV it is) to keep the code small.
        XCTAssertEqual(got.first?.addr?.ip, tv192)
        XCTAssertEqual(got.first?.addr?.port, 47800)
        XCTAssertEqual(got.first?.token, "0badf00d")
        XCTAssertEqual(got.first?.id, "")
    }

    /// A binary PGM (P5) picture: width, height, pixels.
    func readPGM(_ b: Data) throws -> (Int, Int, Data) {
        let bytes = [UInt8](b)
        var fields: [String] = []
        var i = 0
        while fields.count < 4 {
            while bytes[i] == 0x20 || bytes[i] == 0x0a || bytes[i] == 0x0d || bytes[i] == 0x09 { i += 1 }
            let start = i
            while !(bytes[i] == 0x20 || bytes[i] == 0x0a || bytes[i] == 0x0d || bytes[i] == 0x09) { i += 1 }
            fields.append(String(decoding: bytes[start..<i], as: UTF8.self))
        }
        XCTAssertEqual(fields[0], "P5")
        let (w, h) = (Int(fields[1])!, Int(fields[2])!)
        return (w, h, Data(bytes[(i + 1)..<(i + 1 + w * h)]))
    }
}
