// SPDX-License-Identifier: GPL-3.0-or-later
import XCTest
@testable import LuchEngine

/// The menu's plain words: the same kinds of trouble as GNOME's plain.js (gnome-extension/tests/problems.json),
/// why a session ended, and the line about a running session.
final class PlainTests: XCTestCase {
    func testEachRefusalIsToldAsTheKindItIsAsOnGNOME() throws {
        let repo = URL(fileURLWithPath: #filePath).deletingLastPathComponent().deletingLastPathComponent()
            .deletingLastPathComponent().deletingLastPathComponent()
        let data = try Data(contentsOf: repo.appendingPathComponent("gnome-extension/tests/problems.json"))
        let vectors = try XCTUnwrap(JSONSerialization.jsonObject(with: data) as? [[String]])
        XCTAssertGreaterThan(vectors.count, 20)
        for v in vectors { XCTAssertEqual(Plain.kind(v[0]), v[1], v[0]) }
    }

    func testAKnownRefusalSaysWhatToDoAnUnknownOneIsShownAsItIs() {
        XCTAssertEqual(Plain.problem("receiver did not answer hello"),
                       "The TV does not answer. Check that it is on and Luch is open on it, then try again.")
        XCTAssertNil(Plain.problem("portal: Portal request didn't succeed: Cancelled"))
        XCTAssertEqual(Plain.problem("no document on that receiver"), "no document on that receiver")
        for kind in Plain.allKinds where kind != "cancelled" {
            XCTAssertNotEqual(Plain.problem(sample(kind)), sample(kind), kind)
        }
    }

    func testWhySessionEnded() {
        XCTAssertNil(Plain.stopped("menu"))
        XCTAssertNil(Plain.stopped("switch"))
        XCTAssertEqual(Plain.stopped("receiver stopped"), "Stopped on the TV")
        XCTAssertEqual(Plain.stopped("capture: the display went away"),
                       "Stopped: macOS ended the screen recording (the display went away)")
        XCTAssertEqual(Plain.stopped("channel closed"),
                       "Stopped: The TV does not answer. Check that it is on and Luch is open on it, then try again.")
    }

    func testTheSessionLineIsForEveryoneTheNumbersAreInTheTooltip() {
        var s = SessionStats(fps: 59.6, mbps: 18.2, hostMs: 3.1, rttMs: 2.04, decodeMs: 4.0, audioPackets: 0, estMs: 71.4)
        XCTAssertEqual(Plain.session(s).line, "Delay ≈ 71 ms · 60 fps")
        XCTAssertEqual(Plain.session(s).details,
                       "≈ 71 ms: this Mac 3.1 ms, network round trip 2.0 ms, TV decoding 4.0 ms · 60 fps · 18.2 Mbit/s")
        s.battery = 80
        s.charging = true
        s.thermalHeadroom = 0.2
        // A cool TV (headroom rises toward 1 as it heats): its battery only, and only while it is watched.
        XCTAssertEqual(Plain.session(s, watch: true).line, "Delay ≈ 71 ms · 60 fps · TV battery 80%, charging")
        s.thermalHeadroom = 0.8
        XCTAssertEqual(Plain.session(s, watch: true).line,
                       "Delay ≈ 71 ms · 60 fps · TV battery 80%, charging · the TV is hot: lower the quality")
        s.thermalHeadroom = nil
        s.thermal = "severe"
        XCTAssertTrue(Plain.session(s, watch: true).line.hasSuffix(" · the TV is hot: lower the quality"))
    }

    /// Off by default: nothing about the TV's battery or heat in the line, the numbers stay in the tooltip.
    func testAnUnwatchedTvIsNotTalkedAbout() {
        var s = SessionStats(fps: 59.6, mbps: 18.2, hostMs: 3.1, rttMs: 2.04, decodeMs: 4.0, audioPackets: 0, estMs: 71.4)
        s.battery = 40
        s.charging = false
        s.thermal = "severe"
        s.thermalHeadroom = 0.9
        XCTAssertEqual(Plain.session(s).line, "Delay ≈ 71 ms · 60 fps")
        XCTAssertEqual(Plain.session(s).details,
                       "≈ 71 ms: this Mac 3.1 ms, network round trip 2.0 ms, TV decoding 4.0 ms · 60 fps · 18.2 Mbit/s"
                       + " · TV battery 40% · TV thermal: severe · headroom 0.90")
    }

    func testEveryPictureFitHasWords() {
        XCTAssertEqual(Set(SizeMode.allCases.map(\.label)).count, SizeMode.allCases.count)
        XCTAssertTrue(SizeMode.fit.label.contains("Recommended"))
    }

    /// A refusal of each kind, as the receivers and luchd word it.
    private func sample(_ kind: String) -> String {
        [
            "noService": "(is luchd running?)", "sleepOff": "sleep is off on the receiver",
            "sleepOld": "too old to be put to sleep", "wrongCode": "wrong code: try again",
            "codeDigits": "the code is 6 digits", "codeLate": "no code was entered for TV in time",
            "refused": "TV refused the connection", "busy": "TV is busy", "already": "already mirroring to x",
            "playingHere": "TV is playing already: stop it first", "codec": "TV cannot decode HEVC, the picture of this group",
            "update": "this receiver version cannot sleep yet: update it", "unreachable": "receiver went away",
        ][kind] ?? kind
    }
}
