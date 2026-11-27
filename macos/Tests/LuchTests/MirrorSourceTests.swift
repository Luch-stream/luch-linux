// SPDX-License-Identifier: GPL-3.0-or-later
import LuchLocal
import XCTest

/// `luchctl mirror [r] [source] [which]`: luchd's words and the Mac's.
final class MirrorSourceTests: XCTestCase {
    func testLuchdsWordsAndTheMacsMeanTheSame() {
        XCTAssertEqual(MirrorSource.parse("fast"), .asIs)
        XCTAssertEqual(MirrorSource.parse("asis"), .asIs)
        XCTAssertEqual(MirrorSource.parse("virtual"), .extend)
        XCTAssertEqual(MirrorSource.parse("extend"), .extend)
        XCTAssertEqual(MirrorSource.parse("Speed"), .ultra("speed"))
        XCTAssertEqual(MirrorSource.parse("monitor", which: "2"), .monitor("2"))
        XCTAssertEqual(MirrorSource.parse("window"), .window(nil))
        XCTAssertEqual(MirrorSource.parse("audio"), .audio)
        XCTAssertNil(MirrorSource.parse("everything"))
    }

    func testTheWordsAfterMirror() {
        let tv = { (w: String) in w == "Living room TV" || w == "window" }
        XCTAssertTrue(MirrorSource.split([], isReceiver: tv) == (nil, nil, nil))
        XCTAssertTrue(MirrorSource.split(["fast"], isReceiver: tv) == (nil, "fast", nil))
        XCTAssertTrue(MirrorSource.split(["Living room TV"], isReceiver: tv) == ("Living room TV", nil, nil))
        XCTAssertTrue(MirrorSource.split(["Living room TV", "virtual"], isReceiver: tv) == ("Living room TV", "virtual", nil))
        XCTAssertTrue(MirrorSource.split(["monitor", "Studio", "Display"], isReceiver: tv)
                      == (nil, "monitor", "Studio Display"))
        XCTAssertTrue(MirrorSource.split(["Living room TV", "window", "Safari"], isReceiver: tv)
                      == ("Living room TV", "window", "Safari"))
        XCTAssertTrue(MirrorSource.split(["window", "monitor"], isReceiver: tv) == ("window", "monitor", nil),
                      "a TV named like a source word is the TV")
    }

    func testADisplayOrAWindowByNumberOrAPartOfItsName() {
        let all = [ReceiverEntry(id: "1", name: "Built-in Retina Display"),
                   ReceiverEntry(id: "5", name: "Studio Display"),
                   ReceiverEntry(id: "9", name: "Safari — Luch")]
        XCTAssertEqual(SourceChoice.pick("2", from: all)?.id, "5")
        XCTAssertNil(SourceChoice.pick("4", from: all))
        XCTAssertEqual(SourceChoice.pick("safari", from: all)?.id, "9")
        XCTAssertNil(SourceChoice.pick("display", from: all), "two of them")
        XCTAssertEqual(SourceChoice.pick("studio display", from: all)?.id, "5")
        XCTAssertNil(SourceChoice.pick(" ", from: all))
    }
}
