// SPDX-License-Identifier: GPL-3.0-or-later
import CoreGraphics
import XCTest
@testable import LuchMedia

final class VirtualScreenTests: XCTestCase {
    func testA4kTvIsRetinaLikeAndA1080pOneIsNot() {
        let k4 = VirtualScreen.layout(width: 3840, height: 2160)
        XCTAssertTrue(k4.hiDPI)
        XCTAssertEqual(k4.pointsWide, 1920)
        XCTAssertEqual(k4.pointsHigh, 1080)
        let hd = VirtualScreen.layout(width: 1920, height: 1080)
        XCTAssertFalse(hd.hiDPI)
        XCTAssertEqual(hd.pointsWide, 1920)
    }

    func testATvKeepsItsSerial() {
        XCTAssertEqual(VirtualScreen.serial(for: "tv-1"), VirtualScreen.serial(for: "tv-1"))
        XCTAssertNotEqual(VirtualScreen.serial(for: "tv-1"), VirtualScreen.serial(for: "tv-2"))
    }

    /// Makes real screens for a moment (the Mac's display arrangement blinks, and the first time macOS asks whether to
    /// extend or mirror, and holds the screen back until answered), so only on asking, with someone at the Mac:
    /// LUCH_TEST_VIRTUAL_SCREEN=1 swift test --filter VirtualScreenTests
    func testMacOSShowsTheScreenAtTheTvsPixels() throws {
        try XCTSkipUnless(ProcessInfo.processInfo.environment["LUCH_TEST_VIRTUAL_SCREEN"] == "1")
        for (w, h, serial) in [(3840, 2160, 8), (1920, 1080, 7)] as [(Int, Int, UInt32)] {
            let started = Date()
            let v = try XCTUnwrap(VirtualScreen(name: "Luch test", width: w, height: h, fps: 60, serial: serial))
            XCTAssertTrue(v.waitUntilShown(timeout: 60), "\(w)x\(h) never showed")
            print("virtual screen \(w)x\(h) serial \(serial) shown after \(Date().timeIntervalSince(started)) s")
            let mode = try XCTUnwrap(CGDisplayCopyDisplayMode(v.displayID))
            XCTAssertEqual(mode.pixelWidth, w)
            XCTAssertEqual(mode.pixelHeight, h)
            XCTAssertEqual(mode.refreshRate, 60, accuracy: 0.5)
        }
    }
}
