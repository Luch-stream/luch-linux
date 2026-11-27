// Waking a receiver by Bluetooth through another Luch device that is on (PROTOCOL.md 4l, proposed): this Mac cannot
// put manufacturer data on the air (Bluetooth.swift says why), an Android receiver or phone running Luch can. It is
// asked over its control channel to send the remote's advertisement for a moment:
//
//   sender   → {"t":"ble_wake","company":70,"data":"<hex>","ms":2000}
//   receiver → {"t":"ble_wake","ok":true}        once it has been on the air, or
//   receiver → {"t":"error","for":"ble_wake","code":"no_ble"|"not_allowed"|"unknown",...}
//
// Only a receiver whose welcome lists "ble_wake" in `modes` is asked.
// SPDX-License-Identifier: GPL-3.0-or-later
import Foundation
import LuchNet
import LuchProto

public enum WakeRelay {
    public static let mode = "ble_wake"

    /// The request, as the receiver reads it.
    public static func request(_ data: [UInt8], holdMs: Int) -> [String: Any] {
        ["t": mode, "company": Int(BLE.mediatek), "data": hexString(data), "ms": holdMs]
    }

    /// What a receiver answered, as a note for the person: nil when it sent the advertisement.
    public static func refusal(_ answer: [String: Any]?, from name: String) -> String? {
        guard let answer else { return "\(name) did not answer" }
        if answer["t"] as? String == mode, answer["ok"] as? Bool == true { return nil }
        switch answer["code"] as? String {
        case "no_ble": return "\(name) has no Bluetooth that can advertise"
        case "not_allowed": return "\(name) must allow this Mac first (pair with it)"
        case "unknown": return "\(name) cannot send it yet: update Luch on it"
        default: return "\(name): \(answer["detail"] as? String ?? answer["code"] as? String ?? "refused")"
        }
    }

    /// Asks the first of `relays` that can to put `data` on the air for `holdMs`; returns the one that did.
    public static func ask(_ relays: [Receiver], data: [UInt8], holdMs: Int, me: ProbeIdentity) throws -> String {
        var notes: [String] = []
        for r in relays {
            guard let ctl = ControlChannel.open(ip: r.address, port: r.port, interfaceIndex: r.interfaceIndex, timeout: 2) else {
                notes.append("\(r.name) cannot be reached")
                continue
            }
            defer { ctl.bye() }
            ctl.send(["t": "hello", "v": Int(Proto.version), "id": me.id, "name": me.name, "app": Proto.app,
                      "want": [String]()])
            guard let welcome = ctl.waitFor("welcome", timeout: ProbeTimes.welcome) else {
                notes.append("\(r.name) did not answer")
                continue
            }
            guard (welcome["modes"] as? [String])?.contains(mode) == true else {
                notes.append("\(r.name) cannot send it (no \"\(mode)\" in its welcome)")
                continue
            }
            ctl.send(request(data, holdMs: holdMs))
            let answer = ctl.waitForAny([mode, "error"], timeout: Double(holdMs) / 1000 + 3)
            if let why = refusal(answer, from: r.name) {
                notes.append(why)
                continue
            }
            return r.name
        }
        throw FindError(notes.isEmpty ? "no other Luch device is on to send it" : notes.joined(separator: "; "))
    }
}
