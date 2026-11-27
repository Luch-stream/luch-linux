// What luchctl says about finding and waking receivers: the same lines as the Linux luchctl's.
// SPDX-License-Identifier: GPL-3.0-or-later
import Foundation

/// A refusal said to the person as it is.
public struct Refusal: Error, LocalizedError {
    public let message: String
    public init(_ message: String) { self.message = message }
    public var errorDescription: String? { message }
}

public enum FindText {
    /// What changed in a find progress since the last one, as lines for a person: each rung that started or ended.
    public static func progressLines(before: [String: Any], now: [String: Any]) -> [String] {
        func rungs(_ v: [String: Any]) -> [[String: Any]] { v["rungs"] as? [[String: Any]] ?? [] }
        func s(_ v: [String: Any], _ k: String) -> String { v[k] as? String ?? "" }
        let old = rungs(before)
        var out: [String] = []
        for r in rungs(now) {
            let same = old.contains { s($0, "rung") == s(r, "rung") && s($0, "state") == s(r, "state") && s($0, "note") == s(r, "note") }
            let state = s(r, "state")
            if same || state == "waiting" || state == "skipped" { continue }
            let word = ["running": "trying", "done": "done", "failed": "could not"][state] ?? state
            let rung = s(r, "rung").isEmpty ? "?" : s(r, "rung")
            out.append(String(repeating: " ", count: max(0, 5 - rung.count)) + "\(rung) \(word): \(s(r, "note"))")
        }
        if before["running"] as? Bool == true, now["running"] as? Bool == false {
            switch (now["found"] as? NSNumber)?.intValue ?? 0 {
            case 0: out.append("nothing found; `luchctl add <address>` takes a receiver by hand")
            case 1: out.append("1 receiver found")
            case let n: out.append("\(n) receivers found")
            }
        }
        return out
    }

    /// `luchctl known`: one line per receiver talked to, and what to do for one that cannot be woken yet.
    public static func known(_ rows: [[String: Any]]) -> [String] {
        if rows.isEmpty { return ["(none yet: Luch remembers a receiver once it has connected to it)"] }
        func pad(_ s: String, _ n: Int) -> String { s.count >= n ? s : s + String(repeating: " ", count: n - s.count) }
        var out: [String] = []
        for r in rows {
            let wake = r["wake"] as? [String] ?? []
            out.append([pad(r["id"] as? String ?? "", 38), pad(r["name"] as? String ?? "", 20),
                        pad(r["model"] as? String ?? "", 16), pad(r["online"] as? Bool == true ? "on" : "off", 8),
                        "wake: " + (wake.isEmpty ? "-" : wake.joined(separator: ", "))].joined(separator: " "))
            if let h = r["hint"] as? String { out.append(pad("", 38) + " " + h) }
        }
        return out
    }

    /// A known receiver by id or name; with no key, the only one.
    public static func pickKnown(_ rows: [[String: Any]], _ key: String?) throws -> [String: Any] {
        let hits = rows.filter { r in
            guard let key else { return true }
            return r["id"] as? String == key || (r["name"] as? String)?.caseInsensitiveCompare(key) == .orderedSame
        }
        if hits.count == 1 { return hits[0] }
        if let key {
            throw Refusal(hits.isEmpty ? "Luch has not talked to '\(key)' yet (see `luchctl known`)"
                                       : "'\(key)' is ambiguous, use the id")
        }
        throw Refusal(rows.isEmpty ? "no known receivers: connect to one once while it is on"
                                   : "\(rows.count) known receivers, name one (see `luchctl known`)")
    }
}
