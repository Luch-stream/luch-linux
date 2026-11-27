// What luchctl reads in the app's answers about pairing (PROTOCOL.md 3a), as luchctl on Linux reads luchd's.
// SPDX-License-Identifier: GPL-3.0-or-later
import Foundation

/// The receiver in the app's "asks for pairing ... `luchctl pair <id>`" refusal.
public func pairingID(_ msg: String) -> String? {
    guard msg.contains("asks for pairing"), let start = msg.range(of: "`luchctl pair ") else { return nil }
    let rest = msg[start.upperBound...]
    return rest.firstIndex(of: "`").map { String(rest[..<$0]) }
}

/// Whether a word is a pairing code (six digits) rather than a receiver's name.
public func isPairCode(_ w: String) -> Bool {
    w.utf8.count == 6 && w.utf8.allSatisfy { (0x30...0x39).contains($0) }
}
