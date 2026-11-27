// TVs without Luch next to the Luch receivers: one row per TV in the list, and the cast commands for the rows that
// are such a TV (or that `transport` sends to one, or that one plays for).
// SPDX-License-Identifier: GPL-3.0-or-later
import Foundation
import LuchCast
import LuchNet
import LuchTV

extension Caster {
    /// One row per TV, as luchd's Receivers: a Luch receiver as it was found, a TV without Luch as a row of its own
    /// unless a Luch receiver (or a better way) is at its address.
    func rows(_ luch: [Receiver]) -> [Receiver] {
        let byID = Dictionary(luch.map { ($0.id, $0) }, uniquingKeysWith: { a, _ in a })
        return tvs.groups(luch: Self.entries(luch)).compactMap { g in
            let best = g[0]
            if best.kind == .luch { return byID[best.id] }
            return Receiver(id: best.id, name: best.name, model: best.model, address: best.address, interfaceName: "",
                            interfaceIndex: 0, port: best.port, service: best.kind.rawValue)
        }
    }

    static func entries(_ receivers: [Receiver]) -> [Entry] {
        receivers.filter { TVKind.of($0.id) == .luch }.map {
            Entry(kind: .luch, id: $0.id, name: $0.name, model: $0.model, address: $0.address, port: $0.port)
        }
    }

    /// The answer when `row` is a TV without Luch's to handle; nil leaves it to the Luch receiver.
    func tvCommand(_ request: [String: Any], row: Receiver, receivers: [Receiver]) -> [String: Any]? {
        tvs.handle(request, row: row.id, luch: Self.entries(receivers), files: files)
    }
}
