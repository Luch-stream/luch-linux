// SPDX-License-Identifier: GPL-3.0-or-later
import XCTest

/// luchctl's tab completion, through the binary as a shell calls it: `luchctl __complete <words…> <current>`.
final class CompleteTests: XCTestCase {
    private func complete(_ words: String...) throws -> [String] {
        let bin = Bundle(for: Self.self).bundleURL.deletingLastPathComponent().appendingPathComponent("luchctl")
        try XCTSkipUnless(FileManager.default.isExecutableFile(atPath: bin.path), "luchctl not built")
        let p = Process()
        p.executableURL = bin
        p.arguments = ["__complete"] + words
        // No app to ask for receivers: an empty socket directory keeps the answer to the words alone.
        p.environment = ["HOME": NSTemporaryDirectory() + "luch-complete-\(UUID().uuidString)"]
        let out = Pipe()
        p.standardOutput = out
        try p.run()
        p.waitUntilExit()
        let text = String(decoding: out.fileHandleForReading.readDataToEndOfFile(), as: UTF8.self)
        return text.split(separator: "\n").map { String($0.split(separator: "\t").first ?? "") }
    }

    func testCommandsThenEachCommandsOwnWords() throws {
        let top = try complete("")
        XCTAssertTrue(top.contains("mirror") && top.contains("find") && top.contains("calibrate"))
        XCTAssertEqual(try complete("find", ""), ["auto", "all", "mdns", "scan", "ble", "sound", "qr", "direct"])
        XCTAssertEqual(try complete("quiet", ""), ["ask", "on", "off"])
        XCTAssertEqual(try complete("mirror", "--codec", ""), ["auto", "h264", "hevc"])
    }

    func testAListAfterCommasOffersWhatIsNotChosenYet() throws {
        XCTAssertEqual(try complete("find", "mdns,s"), ["mdns,scan", "mdns,ble", "mdns,sound", "mdns,qr", "mdns,direct"])
    }

    func testFilesWhereAFileMayStand() throws {
        XCTAssertEqual(try complete("cast", ""), [":files"])
        XCTAssertEqual(try complete("cast", "--sub", ""), [":files"])
        XCTAssertEqual(try complete("latency", "analyze", ""), [":files"])
    }

    func testAGroupMembersRole() throws {
        XCTAssertEqual(try complete("group", "watch", "Kitchen="), ["Kitchen=video", "Kitchen=audio", "Kitchen=both"])
    }
}
