// Play on Several TVs: the dialog's ready choices become the right group, and a playing group reads in plain words;
// the same expectations as GNOME's gnome-extension/tests/group.test.mjs.
// SPDX-License-Identifier: GPL-3.0-or-later
import XCTest
@testable import LuchLocal
@testable import LuchProto

final class GroupChoiceTests: XCTestCase {
    private let tvs: [(id: String, name: String)] = [("a1", "Living room"), ("b2", "Kitchen"), ("c3", "Bedroom")]

    private func plan(_ c: GroupChoice) -> (ids: [String], roles: [GroupRole], mode: SyncMode, soundOnly: Bool)? {
        guard let p = try? c.plan().get() else { return nil }
        return (p.members.map(\.id), p.members.map(\.role), p.mode, p.soundOnly)
    }

    func testEveryTVIsTickedAndThisScreenGoesToAllOfThemInStep() {
        let p = plan(GroupChoice(tvs))!
        XCTAssertEqual(p.ids, ["a1", "b2", "c3"])
        XCTAssertEqual(p.roles, [.both, .both, .both])
        XCTAssertEqual(p.mode, .watching)
        XCTAssertFalse(p.soundOnly)
    }

    func testMusicInEveryRoomIsSoundOnlyEveryTVASpeakerAlwaysInStep() {
        var c = GroupChoice(tvs)
        c.preset = .music
        c.timing = .interactive
        let p = plan(c)!
        XCTAssertEqual(p.roles, [.audio, .audio, .audio])
        XCTAssertEqual(p.mode, .watching)
        XCTAssertTrue(p.soundOnly)
    }

    func testScreenHereSoundThereLeadsWithThePictureTV() {
        var c = GroupChoice(tvs)
        c.preset = .split
        c.setPicture("c3")
        var p = plan(c)!
        XCTAssertEqual(p.ids, ["c3", "a1", "b2"])
        XCTAssertEqual(p.roles, [.video, .audio, .audio])
        XCTAssertEqual(p.mode, .interactive)
        // Its TV unticked: the first one still ticked takes the picture.
        c.tick("c3", false)
        p = plan(c)!
        XCTAssertEqual(p.ids, ["a1", "b2"])
        XCTAssertEqual(p.roles.first, .video)
    }

    func testOneTVIsNotAGroup() {
        var c = GroupChoice(tvs)
        c.tick("b2", false)
        c.tick("c3", false)
        guard case .failure(let why) = c.plan() else { return XCTFail("one TV played") }
        XCTAssertEqual(why.description, "Tick two TVs or more")
        c.tick("c3", true)
        XCTAssertNotNil(plan(c))
    }

    func testThePlayingGroupInPlainWords() {
        let g: [String: Any] = ["mode": "watch", "d_video_ms": 240, "d_audio_ms": 240]
        XCTAssertEqual(GroupWords.delay(g), "All in step, 0.24 s behind this screen")
        XCTAssertEqual(GroupWords.delay(["mode": "watch", "d_audio_ms": 0]), "Matching the TVs to each other…")
        XCTAssertEqual(GroupWords.delay(["mode": "interactive", "d_video_ms": 0, "d_audio_ms": 2]),
                       "Picture and sound right away")
        XCTAssertEqual(GroupWords.delay(["mode": "interactive", "d_video_ms": 0, "d_audio_ms": 34]),
                       "Picture right away, sound waits 30 ms for it")
        XCTAssertEqual(GroupWords.role("audio"), "sound")
        XCTAssertEqual(GroupWords.role("both"), "picture and sound")
        XCTAssertEqual(GroupWords.title(["Living room", "Kitchen"]), "Together: Living room, Kitchen")
        for p in GroupPreset.allCases {
            XCTAssertNil((p.title + p.detail).range(of: "role|interactive|watch|group|sync|latency|codec",
                                                    options: [.regularExpression, .caseInsensitive]), p.rawValue)
        }
    }

    func testGroupAddTakesAReceiverAndMaybeItsRole() throws {
        XCTAssertEqual(try GroupArgs.member("Den TV").0, "Den TV")
        XCTAssertNil(try GroupArgs.member("Den TV").1)
        XCTAssertEqual(try GroupArgs.member("Kitchen=audio").1, "audio")
        XCTAssertThrowsError(try GroupArgs.member("Kitchen=loud"))
    }
}
