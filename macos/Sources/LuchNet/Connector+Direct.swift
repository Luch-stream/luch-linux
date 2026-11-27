// A receiver paired over Bluetooth before this Mac knew its whole id (its card has only the first 8 hex digits):
// the pin moves to the id its welcome gives, where every later connection looks for it.
// SPDX-License-Identifier: GPL-3.0-or-later
import Foundation

extension Connector {
    /// Moves the pin kept under `from` to `to`; false when there was none.
    @discardableResult
    public func repin(from: String, to: String) throws -> Bool {
        guard from != to, let fp = Pins.fingerprint(from, dir: dir) else { return false }
        let name = Pins.all(dir: dir)[from]?["name"] as? String ?? to
        try Pins.pin(to, fp: fp, name: name, dir: dir)
        try Pins.forget(from, dir: dir)
        return true
    }
}
