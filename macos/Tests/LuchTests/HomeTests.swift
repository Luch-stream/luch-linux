// SPDX-License-Identifier: GPL-3.0-or-later
import Foundation
import XCTest
@testable import LuchLocal
@testable import LuchProto

/// LUCH_HOME moves everything a test instance keeps, its control socket included, away from the installed app's.
final class HomeTests: XCTestCase {
    func testLuchHomeMovesStateAndSocket() {
        let dir = FileManager.default.temporaryDirectory.appendingPathComponent("luch-home-\(UUID().uuidString)").path
        setenv("LUCH_HOME", dir, 1)
        defer { unsetenv("LUCH_HOME") }
        XCTAssertTrue(Home.isolated)
        XCTAssertEqual(Home.support.path, dir)
        XCTAssertTrue(FileManager.default.fileExists(atPath: dir))
        XCTAssertEqual(defaultSocketPath(), dir + "/control.sock")
        XCTAssertEqual(Home.logs.path, dir + "/logs")
        unsetenv("LUCH_HOME")
        XCTAssertFalse(Home.isolated)
        XCTAssertTrue(Home.support.path.hasSuffix("Library/Application Support/Luch"))
    }
}
