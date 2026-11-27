// Which receiver a command means when it names none and there are several: a numbered list, a number or the start of
// a name to pick, Enter for the one picked last time.
// SPDX-License-Identifier: GPL-3.0-or-later
import Foundation

public struct ReceiverEntry: Equatable {
    public var id: String
    public var name: String
    public init(id: String, name: String) {
        self.id = id
        self.name = name
    }
}

public enum ReceiverChoice {
    /// The list as printed, the last one picked marked with a star.
    public static func menu(_ all: [ReceiverEntry], last: String?) -> String {
        all.enumerated().map { i, r in "  \(i + 1)) \(r.name)\(r.id == last ? "  *" : "")" }.joined(separator: "\n")
    }

    /// The receiver an answer picks: its number, the start of its name (one only), or nothing for the last one.
    public static func pick(_ answer: String, from all: [ReceiverEntry], last: String?) -> ReceiverEntry? {
        let a = answer.trimmingCharacters(in: .whitespacesAndNewlines)
        if a.isEmpty { return all.first { $0.id == last } }
        if let n = Int(a) { return (1...all.count).contains(n) ? all[n - 1] : nil }
        let matches = all.filter { $0.name.lowercased().hasPrefix(a.lowercased()) }
        if let exact = matches.first(where: { $0.name.lowercased() == a.lowercased() }) { return exact }
        return matches.count == 1 ? matches[0] : nil
    }

    static var lastFile: URL {
        FileManager.default.homeDirectoryForCurrentUser
            .appendingPathComponent("Library/Application Support/Luch/last-receiver")
    }

    public static var last: String? {
        (try? String(contentsOf: lastFile, encoding: .utf8))?.trimmingCharacters(in: .whitespacesAndNewlines)
    }

    public static func remember(_ id: String) {
        try? (id + "\n").write(to: lastFile, atomically: true, encoding: .utf8)
    }

    /// Asks on the terminal; nil when there is no terminal to ask on, or no answer that picks one.
    public static func ask(_ all: [ReceiverEntry]) -> ReceiverEntry? {
        guard isatty(STDIN_FILENO) != 0, isatty(STDOUT_FILENO) != 0 else { return nil }
        let last = Self.last
        print("Which receiver?\n" + menu(all, last: last))
        while true {
            print(last.flatMap { l in all.first { $0.id == l } }.map { "number or name [\($0.name)]: " } ?? "number or name: ",
                  terminator: "")
            fflush(stdout)
            guard let line = readLine() else { return nil }
            if let r = pick(line, from: all, last: last) {
                remember(r.id)
                return r
            }
            print("no such receiver")
        }
    }
}
