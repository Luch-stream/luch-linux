// luchctl calibrate: a playing group's receivers measured by this Mac's microphone and set to sound together.
// SPDX-License-Identifier: GPL-3.0-or-later
import Foundation
import LuchLocal

let calibrateUsage = """
       | calibrate [self]
       calibrate: while a group plays, each TV whistles in turn and this Mac's microphone times it; each gets the
       offset that makes all sound together (keep the room quiet, about 3 s per TV); self: this Mac's own speaker
       to its own microphone, what the Mac adds to every TV's number
"""

/// `luchctl calibrate`.
func calibrateCommand(_ args: [String]) {
    if args.count == 2, args[1] == "self" {
        print("this Mac whistles four times on its own speaker…")
        let r = (try? LocalClient.request(["cmd": "calibrate", "self": true], timeout: 30)) ?? ["error": "Luch.app is not running: start it with `open -a Luch`"]
        fail(r)
        let f = { (k: String) in (r[k] as? NSNumber).map { String(format: "%.1f ms", $0.doubleValue) } ?? "–" }
        print("heard \(f("loop_ms")) after it was played, the microphone's own \(f("input_latency_ms")) taken off (each: \((r["each_ms"] as? [NSNumber] ?? []).map { String(format: "%.2f", $0.doubleValue) }.joined(separator: " ")))")
        print("Core Audio says the speaker adds \(f("output_latency_ms")): the two should agree within a ms or two; detector: \(r["why"] as? String ?? "?"), peak \(r["peak"] ?? "?")")
        return
    }
    guard args.count == 1 else {
        print("usage: luchctl calibrate [self]")
        exit(2)
    }
    print("calibrating: each TV whistles in turn, keep the room quiet…")
    // About 3.3 s per receiver and a second retry for some: a few minutes is never reached.
    let r = (try? LocalClient.request(["cmd": "calibrate"], timeout: 300)) ?? ["error": "Luch.app is not running: start it with `open -a Luch`"]
    fail(r)
    for line in CalibrateText.lines(r["results"] as? [[String: Any]] ?? []) { print(line) }
}
