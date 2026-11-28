// SPDX-License-Identifier: GPL-3.0-or-later
import XCTest
@testable import LuchLocal

final class FileShowTests: XCTestCase {
    let living = ShowTarget(id: "id-1", name: "Living room TV", presents: true)
    let kitchen = ShowTarget(id: "cast:2", name: "Kitchen", presents: false)

    func testOneTVIsOneClick() {
        let photos = ["/p/a.jpg", "/p/B.HEIC"]
        XCTAssertEqual(FileShow.plan([.show], files: photos, receivers: [living], canWait: true),
                       .run(ShowChoice(title: "Show on Living room TV", verb: .show, receiver: "id-1")))
        let r = FileShow.request(ShowChoice(title: "", verb: .show, receiver: "id-1"), files: photos)
        XCTAssertEqual(r["cmd"] as? String, "cast")
        XCTAssertEqual(r["items"] as? [String], photos, "several photos are one cast: a slideshow")
        XCTAssertEqual(r["id"] as? String, "id-1")
    }

    func testSeveralTVsAreAMenu() {
        guard case .ask(let c) = FileShow.plan([.show], files: ["/p/film.mkv"], receivers: [living, kitchen],
                                               canWait: true) else { return XCTFail() }
        XCTAssertEqual(c.map(\.title), ["Show on Living room TV", "Show on Kitchen"])
        XCTAssertEqual(c[1].receiver, "cast:2")
    }

    func testPresentingIsForOneDeckOnALuchReceiver() {
        XCTAssertEqual(FileShow.plan([.present], files: ["/p/talk.key"], receivers: [living, kitchen], canWait: false),
                       .run(ShowChoice(title: "Present on Living room TV…", verb: .present, receiver: "id-1")),
                       "a TV without Luch cannot present")
        let r = FileShow.request(ShowChoice(title: "", verb: .present, receiver: "id-1"), files: ["/p/talk.key"])
        XCTAssertEqual(r["cmd"] as? String, "present")
        XCTAssertEqual(r["file"] as? String, "/p/talk.key")
        // A deck dropped on the icon: shown or presented.
        guard case .ask(let c) = FileShow.plan([.show, .present], files: ["/p/deck.pdf"], receivers: [living],
                                               canWait: false) else { return XCTFail() }
        XCTAssertEqual(c.map(\.verb), [.show, .present])
        // Photos dropped: only shown.
        XCTAssertEqual(FileShow.plan([.show, .present], files: ["/p/a.png"], receivers: [living], canWait: false),
                       .run(ShowChoice(title: "Show on Living room TV", verb: .show, receiver: "id-1")))
    }

    func testWhatIsSaid() {
        XCTAssertEqual(FileShow.plan([.show], files: ["/p/a.jpg"], receivers: [], canWait: true), .wait,
                       "a service may have just started the app")
        guard case .tell(let none) = FileShow.plan([.show], files: ["/p/a.jpg"], receivers: [], canWait: false),
              case .tell(let zip) = FileShow.plan([.show], files: ["/p/x.zip"], receivers: [living], canWait: false),
              case .tell(let two) = FileShow.plan([.present], files: ["/p/a.pdf", "/p/b.pdf"], receivers: [living],
                                                  canWait: false),
              case .tell(let notLuch) = FileShow.plan([.present], files: ["/p/a.pdf"], receivers: [kitchen],
                                                      canWait: false)
        else { return XCTFail() }
        XCTAssertTrue(none.hasPrefix("No TV found"))
        XCTAssertTrue(zip.hasSuffix("not x.zip"))
        XCTAssertEqual(two, "Present takes one PDF or presentation")
        XCTAssertTrue(notLuch.contains("not Luch receivers"))
        XCTAssertFalse(FileShow.accepts([]))
        XCTAssertFalse(FileShow.accepts(["/p/a.jpg", "/p/folder"]))
    }
}
