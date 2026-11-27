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
            if let eq = a.range(of: "=", options: .backwards) {
                let role = String(a[eq.upperBound...])
                guard ["video", "audio", "both"].contains(role) else {
                    throw Bad(description: "role video|audio|both, not \(role)")
                }
                members.append((String(a[..<eq.lowerBound]), role))
            } else {
                members.append((a, nil))
            }
        }
        guard members.count >= 2 else { throw Bad(description: "a group has two or more receivers") }
        return (members, source)
    }
}
