// SPDX-License-Identifier: GPL-3.0-or-later
import Foundation
import MediaPlayer
import XCTest
@testable import LuchEngine

final class NowPlayingTests: XCTestCase {
    func testALineBecomesTheProtocolMessage() throws {
        let line = """
        {"album":"Album","app":"Music","art":"/tmp/luch-now-playing-1-1","artist":"Artist","at":1000,"dur":215,\
        "elapsed":12.5,"playing":true,"present":true,"rate":1,"title":"Song"}
        """
        let info = try XCTUnwrap(NowPlayingInfo(line: line))
        let m = info.message(now: Date(timeIntervalSince1970: 1002), art: "http://192.168.1.20:47810/t/cover")
        XCTAssertEqual(m["t"] as? String, "now_playing")
        XCTAssertEqual(m["state"] as? String, "playing")
        XCTAssertEqual(m["title"] as? String, "Song")
        XCTAssertEqual(m["artist"] as? String, "Artist")
        XCTAssertEqual(m["album"] as? String, "Album")
        XCTAssertEqual(m["app"] as? String, "Music")
        XCTAssertEqual(m["dur"] as? Double, 215)
        XCTAssertEqual(m["pos"] as? Double, 14.5, "two seconds on from the last known position")
        XCTAssertEqual(m["art"] as? String, "http://192.168.1.20:47810/t/cover")
        XCTAssertEqual(m["can"] as? [String], ["play_pause", "next", "previous"])
    }

    func testPausedPositionStandsAndNothingPlayingIsStopped() throws {
        let paused = try XCTUnwrap(NowPlayingInfo(line: #"{"present":true,"playing":false,"elapsed":30,"at":1000,"dur":40}"#))
        XCTAssertEqual(paused.message(now: Date(timeIntervalSince1970: 1100))["pos"] as? Double, 30)
        XCTAssertEqual(paused.message()["state"] as? String, "paused")
        let late = try XCTUnwrap(NowPlayingInfo(line: #"{"present":true,"playing":true,"elapsed":30,"at":1000,"dur":40}"#))
        XCTAssertEqual(late.message(now: Date(timeIntervalSince1970: 1100))["pos"] as? Double, 40, "never past the end")
        let none = try XCTUnwrap(NowPlayingInfo(line: #"{"present":false,"playing":false}"#))
        XCTAssertEqual(none.message().count, 2, "only t and state")
        XCTAssertEqual(none.message()["state"] as? String, "stopped")
        XCTAssertNil(NowPlayingInfo(line: "luch: MediaRemote is not there"))
    }

    /// The real path: this test process says it plays something, the helper inside the system perl reads it back
    /// through MediaRemote, which answers this process itself nothing since macOS 15.4. Silent: nothing plays.
    func testTheHelperSeesWhatThisMacPlays() throws {
        let lib = try XCTUnwrap(Self.builtLibrary(), "libLuchNowPlaying.dylib is built beside the tests")
        let center = MPNowPlayingInfoCenter.default()
        center.nowPlayingInfo = [MPMediaItemPropertyTitle: "Luch test title", MPMediaItemPropertyArtist: "Luch",
                                 MPMediaItemPropertyPlaybackDuration: 215.0,
                                 MPNowPlayingInfoPropertyElapsedPlaybackTime: 12.5,
                                 MPNowPlayingInfoPropertyPlaybackRate: 1.0]
        center.playbackState = .playing
        let target = MPRemoteCommandCenter.shared().togglePlayPauseCommand.addTarget { _ in .success }
        defer {
            MPRemoteCommandCenter.shared().togglePlayPauseCommand.removeTarget(target)
            center.nowPlayingInfo = nil
            center.playbackState = .stopped
        }

        let seen = expectation(description: "the title comes back")
        let watcher = NowPlayingWatcher()
        var got: NowPlayingInfo?
        watcher.onChange = { info in
            if info.title == "Luch test title", got == nil {
                got = info
                seen.fulfill()
            }
        }
        XCTAssertTrue(watcher.start(library: lib))
        defer { watcher.stop() }
        wait(for: [seen], timeout: 10)
        XCTAssertEqual(got?.artist, "Luch")
        XCTAssertEqual(got?.dur, 215)
        XCTAssertEqual(got?.message()["state"] as? String, "playing")
    }

    static func builtLibrary() -> String? {
        let dir = Bundle(for: NowPlayingTests.self).bundleURL.deletingLastPathComponent()
        let path = dir.appendingPathComponent("libLuchNowPlaying.dylib").path
        return FileManager.default.fileExists(atPath: path) ? path : nil
    }
}
