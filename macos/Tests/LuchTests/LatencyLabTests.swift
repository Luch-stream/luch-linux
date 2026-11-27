// SPDX-License-Identifier: GPL-3.0-or-later
import Foundation
import XCTest
@testable import LuchEngine
@testable import LuchProto

/// The lab's pattern and page must be luch-linux's to the bit: the analyser reads both ends' codes the same way.
final class LatencyLabTests: XCTestCase {
    /// The receiver's QR code for this text, as QrCode.java and luch-latency/src/pattern.rs draw it.
    static let goldenText = "LUCH1;R;v=1.0;mo=Living room TV;a=14;hz=60.0"
    static let golden = [
        "11111110010011011011001111111",
        "10000010001110010011001000001",
        "10111010111010000001001011101",
        "10111010011100111000001011101",
        "10111010001001111011001011101",
        "10000010010001011101101000001",
        "11111110101010101010101111111",
        "00000000111011010101000000000",
        "11101111101101000101111000100",
        "01010000001100100110101101110",
        "10100111110001100110101000111",
        "11101000100101011100110011001",
        "10110011100011110100001001001",
        "00001001010110100000101101010",
        "11011011111110000100000111111",
        "11001000111011110111001101000",
        "11000111001101101101111101011",
        "00111101000101101000111101100",
        "10000111011001000100110111011",
        "01110001111101000111100010010",
        "10111110000011110101111111110",
        "00000000111111100001100011010",
        "11111110111110101000101010011",
        "10000010101011001110100011010",
        "10111010101111100110111111010",
        "10111010010100011100000011111",
        "10111010100011001011000011001",
        "10000010101101011100101110010",
        "11111110110001100000110111011",
    ]

    func testTheQrCodeIsTheReceiversBitForBit() {
        XCTAssertEqual(QrCode(Self.goldenText)?.rows, Self.golden)
        // Longer texts: more blocks, and the version bits from 7 on. The FNV-1a of their rows, from luch-latency's
        // Qr::encode for the same texts.
        func fnv(_ rows: [String]?) -> String {
            let h = (rows ?? []).flatMap(\.utf8).reduce(UInt64(1_469_598_103_934_665_603)) {
                ($0 ^ UInt64($1)) &* 1_099_511_628_211
            }
            return String(format: "%016llx", h)
        }
        let meta = "LUCH1;S;v=luch/0.1 (macos);m=asis;c=hevc;b=21.4;r=1920x1080;p=3.7;h=5.6;e=13.5;hz=59.94;x=2"
        XCTAssertEqual(QrCode(meta)?.size, 37)
        XCTAssertEqual(fnv(QrCode(meta)?.rows), "06d71d5f406691b5")
        XCTAssertEqual(QrCode(String(repeating: "x", count: 150))?.size, 45)
        XCTAssertEqual(fnv(QrCode(String(repeating: "x", count: 150))?.rows), "28479ca90468236f")
        XCTAssertEqual(QrCode(String(repeating: "y", count: 300))?.size, 61)
        XCTAssertEqual(fnv(QrCode(String(repeating: "y", count: 300))?.rows), "0c4aaaf2040eab7b")
        // Too long is nil, not a wrong code.
        XCTAssertNil(QrCode(String(repeating: "x", count: 400)))
    }

    func testTheQrCodeSaysHowTheStreamIsSetUp() {
        let stats: [String: Any] = ["sender": ["rtt_ms": 3.7, "host_ms": 5.62, "mbps": 21.4, "encoder": "hevc_videotoolbox",
                                               "res": "1920x1080"], "est_ms": 13.5]
        XCTAssertEqual(LatencyLab.senderMeta(stats: stats, mode: "asis", hz: 59.94, missed: 2),
                       "LUCH1;S;v=\(Proto.app);m=asis;c=hevc;b=21.4;r=1920x1080;p=3.7;h=5.6;e=13.5;hz=59.94;x=2")
        // Not mirroring yet: what is known, nothing made up.
        XCTAssertEqual(LatencyLab.senderMeta(stats: [:], mode: "", hz: nil, missed: nil), "LUCH1;S;v=\(Proto.app)")
        let page = LatencyLab.pageData("LUCH1;S;v=1")
        XCTAssertEqual((page["qr"] as? [String])?.count, QrCode("LUCH1;S;v=1")?.size)
        XCTAssertEqual((page["layout"] as? [String: Any])?["bits"] as? Int, 12)
        XCTAssertEqual((page["layout"] as? [String: Any])?["origin"] as? [Double], [0.03 + 0.15 * 0.38, 0.03 + 0.15 * 0.38])
    }

    func testThePageIsLuchdsOwn() throws {
        let repo = URL(fileURLWithPath: #filePath).deletingLastPathComponent().deletingLastPathComponent()
            .deletingLastPathComponent().deletingLastPathComponent()
        let linux = try String(contentsOf: repo.appendingPathComponent("luchd/src/latency.html"), encoding: .utf8)
        XCTAssertEqual(LatencyPage.html, linux, "copy luchd/src/latency.html into LatencyPage.swift")
    }

    func testServesThePageAndItsDataOnLoopbackOnly() throws {
        let lab = try LatencyLab(id: "tv") { (["sender": ["res": "1280x720"]], "asis") }
        defer { lab.close() }
        XCTAssertTrue(lab.url.hasPrefix("http://127.0.0.1:"))
        func get(_ path: String) throws -> (Int, Data) {
            let url = URL(string: lab.url.dropLast() + path)!
            var out: (Int, Data) = (0, Data())
            let done = expectation(description: path)
            URLSession.shared.dataTask(with: url) { data, response, _ in
                out = ((response as? HTTPURLResponse)?.statusCode ?? 0, data ?? Data())
                done.fulfill()
            }.resume()
            wait(for: [done], timeout: 5)
            return out
        }
        let page = try get("/")
        XCTAssertEqual(page.0, 200)
        XCTAssertTrue(String(decoding: page.1, as: UTF8.self).contains("requestAnimationFrame"))
        let meta = try get("/meta?hz=60.00&miss=0")
        let body = try XCTUnwrap(JSONSerialization.jsonObject(with: meta.1) as? [String: Any])
        XCTAssertTrue((body["text"] as? String)?.hasSuffix(";m=asis;r=1280x720;hz=60.00;x=0") == true, "\(body)")
        XCTAssertEqual(try get("/etc/passwd").0, 404)
    }
}
