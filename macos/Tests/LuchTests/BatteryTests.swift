// SPDX-License-Identifier: GPL-3.0-or-later
import XCTest
import LuchEngine
import LuchProto

final class BatteryTests: XCTestCase {
    private func caps(battery: Int, charging: Bool, thermal: String = "none", powerSave: Bool = false,
                      max: String = "2340x1080") -> ReceiverCaps {
        var device: [String: Any] = ["charging": charging, "thermal": thermal, "power_save": powerSave]
        if battery >= 0 { device["battery"] = battery } // a TV without a battery sends none (PROTOCOL.md 5)
        return ReceiverCaps(welcome: [
            "t": "welcome", "name": "phone", "model": "m", "max": max, "modes": ["mirror"],
            "decoders": ["video/avc", "video/hevc"],
            "hw": ["video": [
                "video/avc": ["decoder": "a", "max": max, "1080p60": true],
                "video/hevc": ["decoder": "b", "max": max, "1080p60": true],
            ], "display": ["type": "phone", "current": "2340x1080@120", "modes": ["2340x1080@120"],
                         "refresh": 60],
                   "device": device],
        ])!
    }

    /// The user decides; we only warn. A client on battery still gets the preset it asked for.
    func testBatteryDoesNotCapThePicture() {
        var options = MirrorOptions()
        options.codec = .auto
        options.sizeMode = .source
        options.preset = .max
        let plan = CodecPlanner.plan(caps: caps(battery: 40, charging: false, max: "3840x2160"),
                                     receiverID: nil, options: options)
        XCTAssertGreaterThan(plan.height, 1080)
    }

    func testOnBatteryIsReported() {
        XCTAssertTrue(caps(battery: 40, charging: false).onBattery)
        XCTAssertFalse(caps(battery: 100, charging: true).onBattery)
        // A TV on mains power: no battery at all, whatever `charging` says.
        XCTAssertFalse(caps(battery: -1, charging: false).onBattery)
        XCTAssertFalse(caps(battery: -1, charging: true).onBattery)
    }

    /// Headroom rises toward 1 (severe throttling): higher is hotter, as on the receiver (PROTOCOL.md 5).
    func testHotIsHighHeadroomOrAWarmStatus() {
        XCTAssertFalse(Heat.isHot(headroom: nil, thermal: nil))
        XCTAssertFalse(Heat.isHot(headroom: 0.0, thermal: "none"))
        XCTAssertFalse(Heat.isHot(headroom: 0.2, thermal: "none"), "0.2 is cool, not hot")
        XCTAssertFalse(Heat.isHot(headroom: 0.69, thermal: "light"))
        XCTAssertTrue(Heat.isHot(headroom: 0.7, thermal: "none"))
        XCTAssertTrue(Heat.isHot(headroom: 1.2, thermal: nil))
        XCTAssertTrue(Heat.isHot(headroom: nil, thermal: "moderate"))
        XCTAssertTrue(Heat.isHot(headroom: 0.1, thermal: "severe"))
        XCTAssertEqual(Proto.hotHeadroom, 0.7)
    }

    func testChargingKeepsThePreset() {
        var options = MirrorOptions()
        options.codec = .auto
        options.sizeMode = .source
        options.preset = .high
        // A 4K-capable client keeps the preset while it is charging.
        let plan = CodecPlanner.plan(caps: caps(battery: 100, charging: true, max: "3840x2160"),
                                     receiverID: nil, options: options)
        XCTAssertGreaterThan(plan.height, 1080)
    }

    func testThermalIsReportedNotEnforced() {
        XCTAssertEqual(caps(battery: 20, charging: false, thermal: "moderate").thermalRank, 2)
        XCTAssertEqual(caps(battery: 20, charging: false, thermal: "critical").thermalRank, 4)
    }
}
