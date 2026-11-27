// TVs without Luch next to the Luch receivers: one row per TV in the list, and the cast commands for the rows that
// are such a TV (or that `transport` sends to one, or that one plays for).
// SPDX-License-Identifier: GPL-3.0-or-later
import Foundation
import LuchCast
import LuchNet
import LuchTV

extension Caster {
    /// One row per TV, as luchd's Receivers: a Luch receiver as it was found, a TV without Luch as a row of its own
    /// unless a Luch receiver (or a better way) is at its address. Luch rows keep the finder's order (the nearest
    /// first, where Bluetooth heard them), the other TVs follow by name.
    func rows(_ luch: [Receiver]) -> [Receiver] {
        let groups = tvs.groups(luch: Self.entries(luch))
        let best = Set(groups.map { $0[0].id })
        let others = groups.map { $0[0] }.filter { $0.kind != .luch }.map {
            Receiver(id: $0.id, name: $0.name, model: $0.model, address: $0.address, interfaceName: "",
                     interfaceIndex: 0, port: $0.port, service: $0.kind.rawValue)
        }
        return luch.filter { best.contains($0.id) } + others
    }

    static func entries(_ receivers: [Receiver]) -> [Entry] {
        receivers.filter { TVKind.of($0.id) == .luch && $0.service != "web" }.map {
            Entry(kind: .luch, id: $0.id, name: $0.name, model: $0.model, address: $0.address, port: $0.port)
        }
    }

    /// The answer when `row` is a TV without Luch's to handle; nil leaves it to the Luch receiver.
    func tvCommand(_ request: [String: Any], row: Receiver, receivers: [Receiver]) -> [String: Any]? {
        if row.service == "web" {
            return ["error": "the web receiver cannot play media: it shows this screen (luchctl mirror)"]
        }
        return tvs.handle(request, row: row.id, luch: Self.entries(receivers), files: files)
    }
}
