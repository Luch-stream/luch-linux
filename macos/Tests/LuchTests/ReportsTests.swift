// SPDX-License-Identifier: GPL-3.0-or-later
import XCTest
@testable import LuchNet

/// Crash and error reports from receivers (PROTOCOL.md 6), as luchd's reports.rs tests them.
final class ReportsTests: XCTestCase {
    func report() -> [String: Any] {
        ["id": "3f0c2a8e-9d1b-4c7e-8a55-0b6f2d9e1c47", "kind": "error", "time": 1, "count": 2,
         "what": "no decoder for video/hevc", "thread": "luch-video", "stack": "java.io.IOException: x\n\tat a.b(C.java:1)",
         "details": ["codec": "video/hevc"], "app": "0.4", "android": "5.1", "sdk": 22,
         "device": "Acme Box", "hardware": "mt8173", "receiver_model": "Acme Box"]
    }

    func testOnlyUUIDsNameFiles() {
        XCTAssertTrue(Reports.validID("3f0c2a8e-9d1b-4c7e-8a55-0b6f2d9e1c47"))
        XCTAssertFalse(Reports.validID("../../.bashrc"))
        XCTAssertFalse(Reports.validID("3f0c2a8e-9d1b-4c7e-8a55-0b6f2d9e1c47/../x"))
        XCTAssertFalse(Reports.validID(""))
    }

    func testTheIssueLinkCarriesTheReport() {
        let url = Reports.issueURL(report())
        XCTAssertTrue(url.hasPrefix(Reports.issues))
        XCTAssertTrue(url.contains("title=Error%3A%20no%20decoder%20for%20video%2Fhevc"))
        XCTAssertTrue(url.contains("Android%3A%205.1%20%28SDK%2022%29"))
        XCTAssertTrue(url.contains("IOException"))
    }

    func testALongStackIsCutToFitALink() {
        var r = report()
        r["stack"] = String(repeating: "\tat x.Y.z(Y.java:1)\n", count: 2000)
        let url = Reports.issueURL(r)
        XCTAssertLessThanOrEqual(url.utf8.count, Reports.maxURL)
        XCTAssertTrue(url.contains("%E2%80%A6")) // the "…" marking the cut
    }

    func testSummaryReadsLikeASentence() {
        XCTAssertEqual(Reports.summary(report()),
                       "Acme Box error: no decoder for video/hevc, 2 times (Luch 0.4, Android 5.1)")
    }

    func testStoredReportsAreListedAndRemoved() throws {
        let base = try tempDir()
        var bad = report()
        bad["id"] = "../evil"
        let (ids, new) = Reports.store(model: "Acme Box", items: [report(), bad], base: base)
        XCTAssertEqual(ids, ["3f0c2a8e-9d1b-4c7e-8a55-0b6f2d9e1c47"])
        XCTAssertEqual(new, 1)
        XCTAssertEqual(Reports.store(model: "Acme Box", items: [report()], base: base).new, 0,
                       "a report offered again is not kept twice")
        XCTAssertEqual(Reports.list(base: base).count, 1)
        XCTAssertTrue(Reports.remove("3f0c2a8e-9d1b-4c7e-8a55-0b6f2d9e1c47", base: base))
        XCTAssertTrue(Reports.list(base: base).isEmpty)
    }
}
