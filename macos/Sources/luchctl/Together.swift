// luchctl group, offset, audio and latency: the same words, flags and output as luch-linux's luchctl.
// SPDX-License-Identifier: GPL-3.0-or-later
import Foundation
import LuchLocal

let labSteps = """
latency lab on: the page opens in the browser, the TV draws its own pattern in a corner.
  1. click the page for full screen, mirror in the mode to test (`luchctl mirror`)
  2. film both screens in one shot, 240 fps, 10 s, the phone still (docs/latency-lab.md)
  3. luchctl latency analyze VIDEO   writes a report to attach to an issue
  luchctl latency off   when done
"""

let togetherUsage = """
       | audio [r] auto|on|off|sync | latency [on|off] [r] | latency analyze <video> [--out prefix] [--fps n]
         [--readout-ms ms] | group interactive|watch <r>[=role] <r>[=role]... [--source fast|monitor|audio]
         | group add <r>[=role] | group remove <r> | offset <r> <ms>
       group: several receivers in step, the first one shows the picture; role video|audio|both (first: both,
       others: audio, a speaker); interactive keeps the picture as fast as alone, watch delays all alike (rooms,
       TV walls); group add: one more receiver into the group that plays (role: the picture and its sound, or a
       speaker when the group plays sound only); group remove: one out, the others play on, the last one out ends
       the group; offset: a receiver's own correction in ms (positive = later), for its output route
       audio: auto (held to match where the sound lags, Android 5 and 6), on, off (picture only), sync (always held)
       latency: photon-to-photon latency with a phone filming both screens at 240 fps (docs/latency-lab.md)
"""

private func failWith(_ text: String, code: Int32 = 1) -> Never {
    print(text)
    exit(code)
}

/// `luchctl group interactive|watch <r>[=role] <r>[=role]... [--source S]`: the first shows the picture (role both
/// unless named), the others are speakers (audio). Waits while each receiver's person is asked to Allow.
func groupCommand(_ args: [String]) {
    switch (args.count, args.count > 1 ? args[1] : "") {
    case (3, "add"):
        do {
            let (key, role) = try GroupArgs.member(args[2])
            let request: [String: Any] = ["cmd": "group_add", "r": key, "role": role ?? ""]
            fail((try? LocalClient.request(request, timeout: 70))
                 ?? ["error": "Luch.app is not running: start it with `open -a Luch`"])
        } catch {
            failWith("error: \(error)")
        }
        return
    case (3, "remove"):
        fail(call(["cmd": "group_remove", "r": args[2]]))
        return
    default:
        break
    }
    guard args.count >= 4 else { failWith(togetherUsage, code: 2) }
    do {
        let (members, source) = try GroupArgs.parse(Array(args.dropFirst(2)))
        let list: [[String: Any]] = members.enumerated().map { i, m in
            ["r": m.0, "role": m.1 ?? (i == 0 ? "both" : "audio")]
        }
        let r = (try? LocalClient.request(["cmd": "group", "mode": args[1], "members": list, "source": source],
                                          timeout: 70 * Double(members.count))) ?? ["error": "Luch.app is not running: start it with `open -a Luch`"]
        fail(r)
    } catch {
        failWith("error: \(error)")
    }
}

/// `luchctl offset <r> <ms>`.
func offsetCommand(_ args: [String]) {
    guard args.count == 3 else { failWith(togetherUsage, code: 2) }
    guard let ms = Double(args[2]) else { failWith("error: offset in ms, not \(args[2])") }
    fail(call(["cmd": "offset", "id": args[1], "ms": ms]))
}

/// `luchctl audio [r] auto|on|off|sync`.
func audioCommand(_ args: [String]) {
    let modes = ["auto", "on", "off", "sync"]
    switch args.count {
    case 2 where modes.contains(args[1]):
        fail(callPicking(["cmd": "audio", "mode": args[1]]))
    case 3 where modes.contains(args[2]):
        fail(call(["cmd": "audio", "id": args[1], "mode": args[2]]))
    default:
        failWith(togetherUsage, code: 2)
    }
}

/// `luchctl latency [on|off] [r]`, and `latency analyze <video> ...` through the analyser (luch-latency).
func latencyCommand(_ args: [String]) {
    let rest = Array(args.dropFirst())
    if rest.first == "analyze" { analyze(Array(rest.dropFirst())) }
    let on: Bool
    let key: String?
    switch rest.count {
    case 0: (on, key) = (true, nil)
    case 1 where rest[0] == "on" || rest[0] == "off": (on, key) = (rest[0] == "on", nil)
    case 1: (on, key) = (true, rest[0])
    case 2 where rest[0] == "on" || rest[0] == "off": (on, key) = (rest[0] == "on", rest[1])
    default: failWith(togetherUsage, code: 2)
    }
    var request: [String: Any] = ["cmd": "latency", "on": on]
    if let key { request["id"] = key }
    let r = call(request)
    fail(r)
    guard on else { return print("latency lab off") }
    let url = r["url"] as? String ?? ""
    print("\(labSteps)\npage: \(url)")
    let open = Process()
    open.executableURL = URL(fileURLWithPath: "/usr/bin/open")
    open.arguments = [url]
    try? open.run()
}

/// The analyser is luch-latency (Rust, from luch-linux: `cargo install --path luch-latency`), which reads the video
/// through ffmpeg (`brew install ffmpeg`); it runs on a Mac as it is.
private func analyze(_ args: [String]) -> Never {
    let here = URL(fileURLWithPath: CommandLine.arguments[0]).resolvingSymlinksInPath().deletingLastPathComponent()
    let paths = [here.path] + (ProcessInfo.processInfo.environment["PATH"] ?? "").split(separator: ":").map(String.init)
        + [FileManager.default.homeDirectoryForCurrentUser.appendingPathComponent(".cargo/bin").path]
    guard let tool = paths.map({ $0 + "/luch-latency" }).first(where: FileManager.default.isExecutableFile(atPath:)) else {
        failWith("error: the analyser is luch-latency: `cargo install --path luch-latency` in luch-linux, and ffmpeg " +
                 "(`brew install ffmpeg`)")
    }
    let p = Process()
    p.executableURL = URL(fileURLWithPath: tool)
    p.arguments = ["analyze"] + args
    do {
        try p.run()
        p.waitUntilExit()
        exit(p.terminationStatus)
    } catch {
        failWith("error: \(tool): \(error.localizedDescription)")
    }
}

/// The group's lines under `luchctl stats`: D, then each member's need, route and correction.
func printGroup() {
    // The answer is the first member's stats line, whose "group" holds D and the members.
    guard let stats = (try? LocalClient.request(["cmd": "group_stats"]))?["group"] as? [String: Any],
          let g = stats["group"] as? [String: Any], !g.isEmpty else {
        return
    }
    let n = { (v: Any?) in (v as? NSNumber).map { String(format: "%.1f", $0.doubleValue) } ?? "–" }
    print("in step: D picture \(n(g["d_video_ms"])) ms, sound \(n(g["d_audio_ms"])) ms")
    for m in g["members"] as? [[String: Any]] ?? [] {
        let gone = m["present"] as? Bool == false ? " (left)" : ""
        print("  \(m["name"] as? String ?? "?") \(m["role"] as? String ?? "")\(gone): need \(n(m["need_ms"])) ms · " +
              "picture \(n(m["video_pts_ms"])) ms · \(m["route"] as? String ?? "–") · offset \(n(m["offset_ms"])) ms")
    }
}
