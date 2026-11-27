// `luchctl mirror [r] [source] [which]`: luchd's words (fast, monitor, window, virtual, audio) and the Mac's own
// (asis, extend, speed, quality, mac) for how the screen reaches the receiver; `which` names a display or a window.
// SPDX-License-Identifier: GPL-3.0-or-later
import Foundation

public enum MirrorSource: Equatable {
    /// This Mac's screen as it is (luchd's `fast`: there the lowest latency path, here the main display).
    case asIs
    /// A second screen for the TV (luchd's `virtual`).
    case extend
    /// The desktop drawn at one of the TV's modes: speed, quality or mac.
    case ultra(String)
    /// One of this Mac's displays, by number or name; nil: asked for when there are several.
    case monitor(String?)
    /// One window, by its app or title; nil: asked for.
    case window(String?)
    /// The sound only, the TV as a speaker.
    case audio

    public static let words: Set<String> = ["fast", "monitor", "window", "virtual", "audio",
                                            "asis", "extend", "speed", "quality", "mac"]

    public static func parse(_ word: String, which: String? = nil) -> MirrorSource? {
        switch word.lowercased() {
        case "fast", "asis": return .asIs
        case "virtual", "extend": return .extend
        case "speed", "quality", "mac": return .ultra(word.lowercased())
        case "monitor": return .monitor(which)
        case "window": return .window(which)
        case "audio": return .audio
        default: return nil
        }
    }

    /// The words after `mirror`: a receiver, then a source and what it names. A source word first that is no
    /// receiver's name is the source (`mirror fast`, `mirror window Safari`), as luchd's luchctl reads it.
    public static func split(_ words: [String], isReceiver: (String) -> Bool)
        -> (receiver: String?, source: String?, which: String?) {
        guard let first = words.first else { return (nil, nil, nil) }
        if Self.words.contains(first.lowercased()), !isReceiver(first) {
            return (nil, first, words.count > 1 ? words.dropFirst().joined(separator: " ") : nil)
        }
        return (first, words.count > 1 ? words[1] : nil,
                words.count > 2 ? words.dropFirst(2).joined(separator: " ") : nil)
    }
}

/// A display or a window picked by its number in a list or a part of its name (one only).
public enum SourceChoice {
    public static func pick(_ answer: String, from all: [ReceiverEntry]) -> ReceiverEntry? {
        let a = answer.trimmingCharacters(in: .whitespacesAndNewlines).lowercased()
        guard !a.isEmpty else { return nil }
        if let n = Int(a) { return n >= 1 && n <= all.count ? all[n - 1] : nil }
        if let exact = all.first(where: { $0.name.lowercased() == a }) { return exact }
        let matches = all.filter { $0.name.lowercased().contains(a) }
        return matches.count == 1 ? matches[0] : nil
    }

    /// Asks on the terminal; nil when there is no terminal to ask on.
    public static func ask(_ what: String, _ all: [ReceiverEntry]) -> ReceiverEntry? {
        guard isatty(STDIN_FILENO) != 0, isatty(STDOUT_FILENO) != 0 else { return nil }
        print("Which \(what)?\n" + ReceiverChoice.menu(all, last: nil))
        while true {
            print("number or name: ", terminator: "")
            fflush(stdout)
            guard let line = readLine() else { return nil }
            if let r = pick(line, from: all) { return r }
            print("no such \(what)")
        }
    }
}
