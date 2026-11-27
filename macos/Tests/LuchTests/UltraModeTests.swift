// SPDX-License-Identifier: GPL-3.0-or-later
import XCTest
@testable import LuchEngine

final class UltraModeTests: XCTestCase {
    /// A 4K projector at 60 and 120 Hz, 1080p up to 240 Hz, 1440p at 120.
    private func projector(decoder: String = "3840x2160") -> ReceiverCaps {
        let welcome: [String: Any] = [
            "t": "welcome", "name": "Living room TV", "modes": ["mirror"],
            "hw": [
                "display": ["current": "3840x2160@60", "refresh": 60,
                            "modes": ["3840x2160@60", "3840x2160@120", "2560x1440@120", "1920x1080@60",
                                      "1920x1080@120", "1920x1080@240", "1280x720@60", "720x480@60"]],
                "video": ["video/hevc": ["max": decoder, "1080p60": true, "2160p60": true]],
            ],
        ]
        return ReceiverCaps(welcome: welcome)!
    }

    func testTheModesOfTheTvAreRead() {
        XCTAssertEqual(projector().displayModes.count, 8)
        XCTAssertEqual(DisplayMode("3840x2160@120"), DisplayMode(width: 3840, height: 2160, hz: 120))
        XCTAssertNil(DisplayMode("auto"))
    }

    func testEachPriorityPicksOneOfTheTvsOwnModes() {
        let tv = projector()
        XCTAssertEqual(UltraMode.choose(.quality, caps: tv, macPixelWidth: 3024)?.description, "3840x2160@120")
        XCTAssertEqual(UltraMode.choose(.likeMac, caps: tv, macPixelWidth: 3024)?.description, "2560x1440@120")
        XCTAssertEqual(UltraMode.choose(.likeMac, caps: tv, macPixelWidth: 3456)?.description, "3840x2160@120")
        XCTAssertEqual(UltraMode.choose(.speed, caps: tv, macPixelWidth: 3024)?.description, "1920x1080@240")
    }

    func testTheDecoderLimitsTheSize() {
        let tv = projector(decoder: "1920x1080")
        XCTAssertEqual(UltraMode.choose(.quality, caps: tv, macPixelWidth: 3024)?.description, "1920x1080@240")
    }

    func testWithoutAListTheCurrentModeIsTaken() {
        let welcome: [String: Any] = ["t": "welcome", "hw": ["display": ["current": "1920x1080@60", "refresh": 60]]]
        let tv = ReceiverCaps(welcome: welcome)!
        XCTAssertEqual(UltraMode.choose(.speed, caps: tv, macPixelWidth: 3024)?.description, "1920x1080@60")
    }
}
