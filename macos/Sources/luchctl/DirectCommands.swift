// luchctl direct [r] and direct off: connecting directly to a receiver heard over Bluetooth (its Wi-Fi Direct
// network, joined as a Wi-Fi network) and going back. It takes this Mac off its Wi-Fi, so it asks here first.
// SPDX-License-Identifier: GPL-3.0-or-later
import Darwin
import Foundation
import LuchLocal

enum DirectCommands {
    static func handles(_ args: [String]) -> Bool { args.first == "direct" }

    static func run(_ args: [String]) -> Int32 {
        do {
            switch args.count {
            case 2 where args[1] == "off": print(try ask(["cmd": "direct_off"], timeout: 60)["said"] as? String ?? "")
            case 1, 2: try connect(args.count == 2 ? args[1] : nil)
            default: throw Refusal("usage: luchctl direct [r] | direct off")
            }
            return 0
        } catch {
            print("error: \(error.localizedDescription)")
            return 1
        }
    }

    private static func ask(_ request: [String: Any], timeout: TimeInterval = 10) throws -> [String: Any] {
        let r: [String: Any]
        do {
            r = try LocalClient.request(request, timeout: timeout)
        } catch {
            throw Refusal(error.localizedDescription)
        }
        if let e = r["error"] as? String { throw Refusal(e) }
        return r
    }

    private static func status() throws -> [String: Any] {
        try ask(["cmd": "direct_status"])["status"] as? [String: Any] ?? [:]
    }

    /// Asks the question on the terminal, starts the attempt in the app and follows it to the end.
    private static func connect(_ key: String?) throws {
        // The app may listen over Bluetooth first, and ask for Location the first time.
        var plan: [String: Any] = ["cmd": "direct_plan"]
        if let key { plan["id"] = key }
        let p = try ask(plan, timeout: 90)
        let id = p["id"] as? String ?? ""
        guard isatty(STDIN_FILENO) != 0 else {
            throw Refusal("it takes this Mac off its Wi-Fi, so it is asked on a terminal (or Find Receivers, Connect Directly)")
        }
        print("\(p["question"] as? String ?? "Connect directly?") [y/N] ", terminator: "")
        fflush(stdout)
        guard ["y", "yes"].contains((readLine() ?? "").trimmingCharacters(in: .whitespaces).lowercased()) else {
            throw Refusal("not connected directly: this Mac stays on its network")
        }
        let before = try status()
        _ = try ask(["cmd": "direct", "id": id, "confirmed": true])
        var said = ""
        // The state of an earlier attempt counts only once this one has started (or ended in other words).
        var started = false
        let startBy = Date().addingTimeInterval(10)
        let until = Date().addingTimeInterval(400)
        while Date() < until {
            Thread.sleep(forTimeInterval: 0.3)
            let s = try status()
            let (state, note) = (s["state"] as? String ?? "", s["note"] as? String ?? "")
            if ["bluetooth", "pairing", "asking", "joining"].contains(state) || state != before["state"] as? String
                || note != before["note"] as? String { started = true }
            guard started else {
                if Date() > startBy { throw Refusal(note.isEmpty ? "Luch.app did not start connecting" : note) }
                continue
            }
            guard note != said, !note.isEmpty else { continue }
            said = note
            switch state {
            case "failed", "idle": throw Refusal(note)
            case "joined":
                print(note)
                return
            case "pairing":
                print(note)
                if let pairID = s["id"] as? String, let why = enterCode(pairID, code: nil) { throw Refusal(why) }
            default:
                print(note + "…")
            }
        }
        throw Refusal("the direct connection took too long")
    }
}
