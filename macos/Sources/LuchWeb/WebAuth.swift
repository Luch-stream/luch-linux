// Who may watch this screen in a browser (the web receiver): a browser that opens the page is shown a 4-digit code
// and this Mac asks its person, showing the same code, whether to let it in (numeric comparison: the browser on the
// TV in front of them, not another one asking at the same moment). An allowed browser keeps a long-lived cookie, so
// it is not asked again; refusals and the pace of questions are limited as on the TV. luchd's web_auth.rs.
// SPDX-License-Identifier: GPL-3.0-or-later
import CryptoKit
import Foundation
import Security

public enum WebAuth {
    /// The cookie an allowed browser keeps.
    public static let cookieName = "luch_web"
    /// Browsers remembered at most; the least recently used goes.
    public static let max = 20
    /// Refusals from an address after which it is not asked about for `ban` seconds.
    public static let strikes = 3
    public static let ban: UInt64 = 3600
    /// How long the question waits for an answer.
    public static let ask = 60.0

    static func unixNow() -> UInt64 { UInt64(Date().timeIntervalSince1970) }

    static func randomHex(_ bytes: Int) -> String {
        var b = [UInt8](repeating: 0, count: bytes)
        _ = SecRandomCopyBytes(kSecRandomDefault, bytes, &b)
        return b.map { String(format: "%02x", $0) }.joined()
    }

    /// Four digits for the page and the question here.
    public static func code() -> String {
        var b = [UInt8](repeating: 0, count: 4)
        _ = SecRandomCopyBytes(kSecRandomDefault, 4, &b)
        let n = b.reduce(UInt32(0)) { $0 << 8 | UInt32($1) }
        return String(format: "%04d", n % 10_000)
    }

    static func hash(_ token: String) -> String {
        SHA256.hash(data: Data(token.utf8)).map { String(format: "%02x", $0) }.joined()
    }

    // MARK: - allowed browsers

    /// web-browsers.json: [{id, hash (of the token), name, address, allowed, used}]. Only the token's hash is kept: the
    /// file does not let anyone in.
    static func load(_ dir: URL) -> [[String: Any]] {
        guard let d = try? Data(contentsOf: dir.appendingPathComponent("web-browsers.json")),
              let v = try? JSONSerialization.jsonObject(with: d) as? [[String: Any]] else { return [] }
        return v
    }

    static func save(_ dir: URL, _ all: [[String: Any]]) throws {
        let d = try JSONSerialization.data(withJSONObject: all, options: [.prettyPrinted, .sortedKeys])
        let f = dir.appendingPathComponent("web-browsers.json")
        try d.write(to: f, options: .atomic)
        try? FileManager.default.setAttributes([.posixPermissions: 0o600], ofItemAtPath: f.path)
    }

    static func used(_ b: [String: Any]) -> UInt64 { (b["used"] as? NSNumber)?.uint64Value ?? 0 }

    /// A new token for a browser just allowed; the least recently used beyond `max` is forgotten.
    public static func allow(_ dir: URL, name: String, address: String) throws -> String {
        let token = randomHex(32)
        var all = load(dir)
        // In front: among browsers used in the same second, the newest stays.
        all.insert(["id": randomHex(8), "hash": hash(token), "name": name, "address": address,
                    "allowed": unixNow(), "used": unixNow()], at: 0)
        all = Array(stableSorted(all).prefix(max))
        try save(dir, all)
        return token
    }

    /// Most recently used first; equal ones keep their order.
    static func stableSorted(_ all: [[String: Any]]) -> [[String: Any]] {
        all.enumerated().sorted { a, b in
            used(a.element) != used(b.element) ? used(a.element) > used(b.element) : a.offset < b.offset
        }.map(\.element)
    }

    /// Whether `token` is an allowed browser's; marks it used.
    public static func known(_ dir: URL, token: String) -> Bool {
        let h = hash(token)
        var all = load(dir)
        guard let i = all.firstIndex(where: { $0["hash"] as? String == h }) else { return false }
        all[i]["used"] = unixNow()
        try? save(dir, all)
        return true
    }

    /// The allowed browsers, most recently used first, without their hashes.
    public static func list(_ dir: URL) -> [[String: Any]] {
        stableSorted(load(dir)).map { $0.filter { $0.key != "hash" } }
    }

    @discardableResult
    public static func forget(_ dir: URL, id: String) throws -> Bool {
        var all = load(dir)
        let before = all.count
        all.removeAll { $0["id"] as? String == id }
        try save(dir, all)
        return all.count != before
    }

    /// The `luch_web` cookie of an HTTP request head, if any.
    public static func cookie(_ head: String) -> String? {
        for line in head.components(separatedBy: "\r\n") where line.lowercased().hasPrefix("cookie:") {
            for c in line.dropFirst(7).split(separator: ";") {
                let kv = c.trimmingCharacters(in: .whitespaces).split(separator: "=", maxSplits: 1)
                if kv.count == 2, kv[0] == cookieName { return String(kv[1]) }
            }
        }
        return nil
    }

    // MARK: - refusals and pace

    public enum Verdict: Equatable {
        case ask
        case banned(UInt64)
        case busy
    }

    /// Refusals per address and the one question at a time, like the TV's Gate.
    public struct Limits {
        private var strikesBy: [String: (count: Int, since: UInt64, until: UInt64)] = [:]
        private var asking = false

        public init() {}

        public mutating func check(_ address: String, now: UInt64) -> Verdict {
            if let s = strikesBy[address], s.until > now { return .banned(s.until - now) }
            if asking { return .busy }
            asking = true
            return .ask
        }

        public mutating func done() {
            asking = false
        }

        public mutating func strike(_ address: String, now: UInt64) {
            var s = strikesBy[address] ?? (0, now, 0)
            if now - s.since >= ban { s = (0, now, 0) }
            s.count += 1
            if s.count >= strikes { s = (0, now, now + ban) }
            strikesBy[address] = s
        }
    }
}
