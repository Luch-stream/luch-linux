// What the menu bar says, in plain words: a refusal with what to do now, why a session ended, and the line about
// a running session. The receivers, the network and the engine answer in short technical English ("receiver went
// away", "receiver did not answer hello"), which says nothing to most people. The kinds are GNOME's
// (gnome-extension/luch@toxblh.github.io/plain.js): PlainTests checks both against gnome-extension/tests/problems.json.
// SPDX-License-Identifier: GPL-3.0-or-later
import Foundation

public enum Plain {
    /// In order: the first that matches wins.
    private static let kinds: [(String, NSRegularExpression)] = [
        // The person closed a picker or a dialog: nothing to tell them.
        ("cancelled", "portal.*cancel|cancelled by the user"),
        ("noService", "is luchd running|ServiceUnknown|NameHasNoOwner|was not provided by any \\.service"),
        ("sleepOff", "sleep is off on the receiver"),
        ("sleepOld", "too old to be put to sleep"),
        ("wrongCode", "wrong code"),
        ("codeDigits", "the code is \\d+ digits"),
        ("codeLate", "no code was entered .* in time"),
        ("refused", "refused the (direct )?connection"),
        ("busy", " is busy\\b"),
        // A group: the receiver plays something of this Mac's already, or cannot decode the group's picture.
        ("playingHere", " is playing already\\b"),
        ("codec", "cannot decode HEVC"),
        ("already", "already (mirroring|connected)"),
        ("update", ": update it\\b|cannot \\w+ yet: update"),
        ("unreachable", "went away|did not answer| is not connected|channel closed|timed out|connection refused|no route to host"
            + "|network is unreachable|host is down|connection reset|broken pipe"),
    ].map { ($0.0, try! NSRegularExpression(pattern: $0.1, options: [.caseInsensitive])) }

    private static let words: [String: String] = [
        "noService": "Luch's service is not running on this computer.",
        "sleepOff": "This TV cannot be turned off from here yet. On the TV, open Luch › Settings › Turn off from a computer.",
        "sleepOld": "This TV cannot be turned off from here: it needs Android 9 or newer.",
        "wrongCode": "Wrong code. The TV shows a new one: do it again and type that.",
        "codeDigits": "The code is the 6 digits on the TV.",
        "codeLate": "The code was not typed in time. Do it again: the TV shows a new code.",
        "refused": "The TV said no. Try again and press Allow on the TV.",
        "busy": "The TV is showing another device. Stop that there, or wait.",
        "playingHere": "This TV is already showing something from this Mac. Stop that first.",
        "codec": "This TV cannot show the picture the way the others already get it. Stop, then start again with it ticked too: a way for all is chosen then.",
        "already": "Already mirroring to this TV.",
        "update": "Luch on the TV is too old for this: update it.",
        "unreachable": "The TV does not answer. Check that it is on and Luch is open on it, then try again.",
    ]

    /// The kind of trouble a refusal is, or "other" for one told as it is.
    public static func kind(_ text: String) -> String {
        let range = NSRange(text.startIndex..., in: text)
        return kinds.first { $0.1.firstMatch(in: text, range: range) != nil }?.0 ?? "other"
    }

    /// Every kind there is, for the tests.
    public static var allKinds: [String] { kinds.map(\.0) }

    /// The refusal for a person: plain words and what to do, the text itself when it is not a known kind, nil when
    /// there is nothing to tell (they cancelled it themselves).
    public static func problem(_ text: String) -> String? {
        let k = kind(text)
        if k == "cancelled" { return nil }
        return words[k] ?? text
    }

    /// Why a session ended, for the menu; nil when the person ended it here (Stop, switching, quitting).
    public static func stopped(_ reason: String) -> String? {
        switch reason {
        case "menu", "switch", "quit", "SIGINT": return nil
        case "receiver stopped": return "Stopped on the TV"
        default:
            if reason.hasPrefix("capture: ") {
                return "Stopped: macOS ended the screen recording (\(reason.dropFirst("capture: ".count)))"
            }
            return "Stopped: " + (problem(reason) ?? reason)
        }
    }

    /// A running session in two lines: `line` for everyone (the delay, the picture's rate, the TV's battery and a
    /// hot TV), `details` the numbers behind it for the tooltip.
    public static func session(_ s: SessionStats) -> (line: String, details: String) {
        var line = String(format: "Delay ≈ %.0f ms · %.0f fps", s.estMs, s.fps)
        if let battery = s.battery { line += " · TV battery \(battery)%" + (s.charging == true ? ", charging" : "") }
        // Warn, never throttle on its own: the person decides whether to lower the quality.
        if let headroom = s.thermalHeadroom, headroom < 0.3 { line += " · the TV is hot: lower the quality" }
        var details = String(format: "≈ %.0f ms: this Mac %.1f ms, network round trip %.1f ms", s.estMs, s.hostMs, s.rttMs)
        if let decode = s.decodeMs { details += String(format: ", TV decoding %.1f ms", decode) }
        details += String(format: " · %.0f fps · %.1f Mbit/s", s.fps, s.mbps)
        if let thermal = s.thermal, thermal != "none" { details += " · TV thermal: \(thermal)" }
        if let headroom = s.thermalHeadroom { details += String(format: " · headroom %.2f", headroom) }
        return (line, details)
    }
}

extension SizeMode {
    /// The menu's words for it: what the person sees on the TV.
    public var label: String {
        switch self {
        case .fit: return "Fit: No Bars, Nothing Cut (Recommended)"
        case .source: return "This Mac's Shape: Bars on the TV"
        case .client: return "Fill the TV: the Edges Cut Off"
        case .average: return "In Between: Thin Bars, Less Cut"
        }
    }
}
