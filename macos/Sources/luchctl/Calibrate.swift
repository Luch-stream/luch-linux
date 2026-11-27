// luchctl calibrate: a playing group's receivers measured by this Mac's microphone and set to sound together.
// SPDX-License-Identifier: GPL-3.0-or-later
import Foundation
import LuchLocal

let calibrateUsage = """
       | calibrate
       calibrate: while a group plays, each TV whistles in turn and this Mac's microphone times it; each gets the
       offset that makes all sound together (keep the room quiet, about 3 s per TV)
"""

/// `luchctl calibrate`.
func calibrateCommand(_ args: [String]) {
    guard args.count == 1 else {
        print("usage: luchctl calibrate")
        exit(2)
    }
    print("calibrating: each TV whistles in turn, keep the room quiet…")
    // About 3.3 s per receiver and a second retry for some: a few minutes is never reached.
    let r = (try? LocalClient.request(["cmd": "calibrate"], timeout: 300)) ?? ["error": "Luch.app is not running"]
    fail(r)
    for line in CalibrateText.lines(r["results"] as? [[String: Any]] ?? []) { print(line) }
}
