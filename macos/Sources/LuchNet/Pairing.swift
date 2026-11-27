// Pairing with receivers (PROTOCOL.md 3a): the sender's half of the code check, and the receivers' certificates
// pinned once paired. The same bytes and files as luchd's pair.rs.
// SPDX-License-Identifier: GPL-3.0-or-later
import CryptoKit
import Foundation
import Security

public enum Pairing {
    public static let digits = 6
    static let nonceSize = 16
    static let label = Array("luch-pair-1".utf8)
    static let sender = UInt8(ascii: "S")
    static let receiver = UInt8(ascii: "R")

    /// SHA-256 of "luch-pair-1" | role | round | digit (ASCII) | nonce (16) | sender fp (32) | receiver fp (32): the
    /// same bytes as the receiver's Pairing.hash.
    static func commitment(role: UInt8, round: Int, digit: UInt8, nonce: [UInt8], sender: Fingerprint,
                           receiver: Fingerprint) -> [UInt8] {
        var h = SHA256()
        h.update(data: label)
        h.update(data: [role, UInt8(round), digit])
        h.update(data: nonce)
        h.update(data: sender)
        h.update(data: receiver)
        return Array(h.finalize())
    }

    /// The code as typed: six digits; spaces and dashes are let through ("123 456").
    public static func cleanCode(_ typed: String) throws -> [UInt8] {
        let digits = Array(typed.utf8).filter { $0 != UInt8(ascii: " ") && $0 != UInt8(ascii: "-") }
        guard digits.count == Self.digits, digits.allSatisfy({ (0x30...0x39).contains($0) }) else {
            throw Failed.io("the code is \(Self.digits) digits, as shown on the TV")
        }
        return digits
    }

    /// Why pairing did not end with both sides pinned.
    public enum Failed: Error, Equatable {
        /// The receiver said no: `pair_refused` with its reason and wait.
        case refused(String, Int)
        /// The receiver's opening did not match: a wrong digit on its side, or someone in between.
        case badReceiver
        case io(String)
    }

    /// The sender's half of the code check: one round per digit, the sender committing and opening first. Ends after
    /// the receiver's `paired`.
    static func codeCheck(_ ctl: ControlChannel, code: [UInt8], me: Fingerprint, receiver: Fingerprint) throws {
        for (round, digit) in code.enumerated() {
            var nonce = [UInt8](repeating: 0, count: nonceSize)
            guard SecRandomCopyBytes(kSecRandomDefault, nonceSize, &nonce) == errSecSuccess else {
                throw Failed.io("no randomness")
            }
            let c = commitment(role: sender, round: round, digit: digit, nonce: nonce, sender: me, receiver: receiver)
            ctl.send(["t": "pair_commit", "round": round, "c": hex(c)])
            let theirs = try expect(ctl, "pair_commit", round: round)
            ctl.send(["t": "pair_open", "round": round, "n": hex(nonce)])
            let open = try expect(ctl, "pair_open", round: round)
            guard let tc = (theirs["c"] as? String).flatMap(unhex), let tn = (open["n"] as? String).flatMap(unhex),
                  tn.count == nonceSize,
                  commitment(role: Self.receiver, round: round, digit: digit, nonce: tn, sender: me,
                             receiver: receiver) == tc else {
                throw Failed.badReceiver
            }
        }
        _ = try expect(ctl, "paired", round: nil)
    }

    /// The next pairing message `t` (for `round`); `pair_refused` ends it with the receiver's reason.
    private static func expect(_ ctl: ControlChannel, _ t: String, round: Int?) throws -> [String: Any] {
        guard let v = ctl.waitForAny([t, "pair_refused"], timeout: 20) else {
            throw Failed.io(ctl.isClosed ? "the receiver closed the connection" : "no \(t) from the receiver")
        }
        if v["t"] as? String == "pair_refused" { throw refused(v) }
        if let round, (v["round"] as? NSNumber)?.intValue != round {
            throw Failed.io("expected \(t) \(round), got \(v["round"] ?? "none")")
        }
        return v
    }

    static func refused(_ v: [String: Any]) -> Failed {
        .refused(v["reason"] as? String ?? "refused", (v["retry_s"] as? NSNumber)?.intValue ?? 0)
    }

    /// A refusal in words, for the person at the sender.
    public static func explain(_ f: Failed, tv: String) -> String {
        func wait(_ s: Int) -> String { s >= 120 ? "\((s + 59) / 60) min" : "\(s) s" }
        switch f {
        case let .refused(reason, s):
            switch reason {
            case "denied": return "\(tv) refused the connection"
            case "banned": return "\(tv) was refused too often from here and ignores this computer for \(wait(s))"
            case "busy": return "\(tv) is already asking about another device; try again in \(wait(s))"
            case "wrong_code": return "wrong code: try again, \(tv) shows a new one"
            case "timeout": return "nobody answered on \(tv) in time"
            case "dismissed": return "the request was closed on \(tv)"
            default: return "\(tv) refused: \(reason)"
            }
        case .badReceiver:
            return "\(tv) failed the code check: the code was mistyped, or another device is in between; nothing was paired"
        case let .io(e):
            return "pairing with \(tv) broke off: \(e)"
        }
    }
}

/// ~/Library/Application Support/Luch/paired.json: receiver id -> {"fp", "name", "paired" (unix time)}, as luchd's.
public enum Pins {
    private static let lock = NSLock()

    public static func all(dir: URL = supportDirectory()) -> [String: [String: Any]] {
        guard let d = try? Data(contentsOf: dir.appendingPathComponent("paired.json")),
              let v = try? JSONSerialization.jsonObject(with: d) as? [String: [String: Any]] else { return [:] }
        return v
    }

    public static func fingerprint(_ id: String, dir: URL = supportDirectory()) -> Fingerprint? {
        guard let fp = (all(dir: dir)[id]?["fp"] as? String).flatMap(unhex), fp.count == 32 else { return nil }
        return fp
    }

    public static func pin(_ id: String, fp: Fingerprint, name: String, dir: URL = supportDirectory()) throws {
        lock.lock()
        defer { lock.unlock() }
        var a = all(dir: dir)
        a[id] = ["fp": hex(fp), "name": name, "paired": Int(Date().timeIntervalSince1970)]
        try write(a, dir)
    }

    /// False when it was not paired.
    @discardableResult
    public static func forget(_ id: String, dir: URL = supportDirectory()) throws -> Bool {
        lock.lock()
        defer { lock.unlock() }
        var a = all(dir: dir)
        guard a.removeValue(forKey: id) != nil else { return false }
        try write(a, dir)
        return true
    }

    private static func write(_ a: [String: [String: Any]], _ dir: URL) throws {
        let d = try JSONSerialization.data(withJSONObject: a, options: [.prettyPrinted, .sortedKeys])
        try d.write(to: dir.appendingPathComponent("paired.json"), options: .atomic)
    }
}
