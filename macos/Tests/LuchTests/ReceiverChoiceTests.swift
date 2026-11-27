// SPDX-License-Identifier: GPL-3.0-or-later
import XCTest
@testable import LuchLocal

final class ReceiverChoiceTests: XCTestCase {
    let all = [ReceiverEntry(id: "a", name: "Living room TV"), ReceiverEntry(id: "b", name: "Living room TV (dev)"),
               ReceiverEntry(id: "c", name: "Bedroom projector")]

    func testANumberANameOrEnterPicks() {
        XCTAssertEqual(ReceiverChoice.pick("2", from: all, last: nil)?.id, "b")
        XCTAssertEqual(ReceiverChoice.pick(" bed ", from: all, last: nil)?.id, "c")
        XCTAssertEqual(ReceiverChoice.pick("living room tv", from: all, last: nil)?.id, "a", "an exact name beats a longer one")
        XCTAssertEqual(ReceiverChoice.pick("", from: all, last: "c")?.id, "c", "Enter: the one picked last time")
    }

    func testWhatPicksNothing() {
        XCTAssertNil(ReceiverChoice.pick("4", from: all, last: nil))
        XCTAssertNil(ReceiverChoice.pick("0", from: all, last: nil))
        XCTAssertNil(ReceiverChoice.pick("Living", from: all, last: nil), "two names start so")
        XCTAssertNil(ReceiverChoice.pick("", from: all, last: nil))
    }

    func testTheMenuMarksTheLastOne() {
        XCTAssertEqual(ReceiverChoice.menu(all, last: "b"),
                       "  1) Living room TV\n  2) Living room TV (dev)  *\n  3) Bedroom projector")
    }
}
