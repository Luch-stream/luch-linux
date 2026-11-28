// `luchctl group`'s words, as luch-linux's luchctl reads them.
// SPDX-License-Identifier: GPL-3.0-or-later
import Foundation

public enum GroupArgs {
    public struct Bad: Error, CustomStringConvertible {
        public let description: String
    }

    /// The receivers (`name` or `name=role`) and the `--source` (default fast).
    public static func parse(_ args: [String]) throws -> (members: [(String, String?)], source: String) {
        var members: [(String, String?)] = []
        var source = "fast"
        var it = args.makeIterator()
        while let a = it.next() {
            if a == "--source" {
                guard let s = it.next() else { throw Bad(description: "--source needs a source") }
                source = s
                continue
            }
            members.append(try member(a))
        }
        guard members.count >= 2 else { throw Bad(description: "a group has two or more receivers") }
        return (members, source)
    }

    /// One receiver, `name` or `name=role` (`group`, and `group add`, where no role is the group's own default).
    public static func member(_ a: String) throws -> (String, String?) {
        guard let eq = a.range(of: "=", options: .backwards) else { return (a, nil) }
        let role = String(a[eq.upperBound...])
        guard ["video", "audio", "both"].contains(role) else {
            throw Bad(description: "role video|audio|both, not \(role)")
        }
        return (String(a[..<eq.lowerBound]), role)
    }
}
