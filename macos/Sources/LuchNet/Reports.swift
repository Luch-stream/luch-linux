// Crash and error reports a receiver keeps and hands to a paired sender that asks (`"want": ["reports"]`;
// PROTOCOL.md 6). Kept in ~/Library/Application Support/Luch/reports, answered with `reports_taken`; the app shows a
// notification that opens a ready-to-file issue. Nothing is sent anywhere without the person. As luchd's reports.rs.
// SPDX-License-Identifier: GPL-3.0-or-later
import Foundation
import LuchProto

public enum Reports {
    /// Where receiver reports are filed: the receiver's repository.
    public static let issues = "https://altlinux.space/Luch/luch-android/issues/new"
    /// A browser takes long URLs, but not endlessly: the stack is cut to keep the link under this.
    static let maxURL = 7000
    private static let lock = NSLock()

    /// New reports came from the receiver named: the newest of them (the app shows it). Called on any thread.
    public static var onNew: ((_ report: [String: Any], _ receiver: String) -> Void)?

    public static func directory(_ base: URL = supportDirectory()) -> URL {
        let d = base.appendingPathComponent("reports", isDirectory: true)
        try? FileManager.default.createDirectory(at: d, withIntermediateDirectories: true)
        return d
    }

    /// A report id is the receiver's UUID; anything else (a path, "..") is refused before it names a file.
    public static func validID(_ id: String) -> Bool {
        id.utf8.count == 36 && id.utf8.allSatisfy { $0 == UInt8(ascii: "-") || ($0 < 128 && isxdigit(Int32($0)) != 0) }
    }

    /// Keeps the reports of a `{"t":"reports","items":[...]}` message; returns the ids to answer `reports_taken` with
    /// (every valid one, also those kept before) and how many were new.
    public static func store(model: String, items: [[String: Any]], base: URL = supportDirectory()) -> (ids: [String], new: Int) {
        lock.lock()
        defer { lock.unlock() }
        let d = directory(base)
        var ids: [String] = []
        var new = 0
        for item in items {
            guard let id = item["id"] as? String, validID(id) else { continue }
            let path = d.appendingPathComponent(id + ".json")
            if !FileManager.default.fileExists(atPath: path.path) {
                var r = item
                r["receiver_model"] = model
                guard let data = try? JSONSerialization.data(withJSONObject: r), (try? data.write(to: path)) != nil else {
                    continue // not kept: let the receiver offer it again
                }
                new += 1
            }
            ids.append(id)
        }
        return (ids, new)
    }

    /// Kept reports, oldest first.
    public static func list(base: URL = supportDirectory()) -> [[String: Any]] {
        let d = directory(base)
        let files = (try? FileManager.default.contentsOfDirectory(at: d, includingPropertiesForKeys: nil)) ?? []
        return files.compactMap { url -> [String: Any]? in
            guard url.pathExtension == "json", let data = try? Data(contentsOf: url) else { return nil }
            return (try? JSONSerialization.jsonObject(with: data)) as? [String: Any]
        }.sorted { time($0) < time($1) }
    }

    private static func time(_ r: [String: Any]) -> Int64 { (r["time"] as? NSNumber)?.int64Value ?? 0 }

    public static func remove(_ id: String, base: URL = supportDirectory()) -> Bool {
        validID(id) && (try? FileManager.default.removeItem(at: directory(base).appendingPathComponent(id + ".json"))) != nil
    }

    /// One line for a list and a notification.
    public static func summary(_ r: [String: Any]) -> String {
        let kind = r["kind"] as? String == "crash" ? "crashed" : "error"
        let count = (r["count"] as? NSNumber)?.intValue ?? 1
        let who = r["receiver_model"] as? String ?? r["device"] as? String ?? "receiver"
        return "\(who) \(kind): \(r["what"] as? String ?? "?")\(count > 1 ? ", \(count) times" : "")"
            + " (Luch \(r["app"] as? String ?? "?"), Android \(r["android"] as? String ?? "?"))"
    }

    /// The issue to file, with the report in its body. The report holds no addresses or names (the receiver scrubs
    /// them), and the person sees everything before sending.
    public static func issueURL(_ r: [String: Any]) -> String {
        let title = "\(r["kind"] as? String == "crash" ? "Crash" : "Error"): \(r["what"] as? String ?? "?")"
        let head = "Reported by the receiver, filed from Luch for macOS.\n\n"
            + "- Luch: \(r["app"] as? String ?? "?")\n- Android: \(r["android"] as? String ?? "?") (SDK \(json(r["sdk"])))\n"
            + "- Device: \(r["device"] as? String ?? "?") (\(r["hardware"] as? String ?? "?"))\n"
            + "- Times: \(json(r["count"]))\n- Thread: \(r["thread"] as? String ?? "?")\n- Session: `\(json(r["details"]))`\n\n"
        var stack = r["stack"] as? String ?? ""
        while true {
            let body = "\(head)```\n\(stack)\n```\n"
            let url = "\(issues)?title=\(encode(title))&body=\(encode(body))"
            if url.utf8.count <= maxURL || stack.isEmpty { return url }
            // Keep the top of the stack: the exception and the frames nearest to it.
            stack = String(stack.prefix(stack.count * 3 / 4)) + "\n…"
        }
    }

    /// A value as JSON writes it ("null" when missing), as luchd's issue body shows it.
    private static func json(_ v: Any?) -> String {
        guard let v, !(v is NSNull) else { return "null" }
        if let s = v as? String { return "\"\(s)\"" }
        if let d = try? JSONSerialization.data(withJSONObject: v, options: [.sortedKeys, .fragmentsAllowed]) {
            return String(decoding: d, as: UTF8.self)
        }
        return "\(v)"
    }

    /// Percent-encoding for a URL query value.
    static func encode(_ s: String) -> String {
        s.utf8.map { b -> String in
            let c = Character(UnicodeScalar(b))
            return b < 128 && (c.isLetter || c.isNumber || "-_.~".contains(c)) ? String(c) : String(format: "%%%02X", b)
        }.joined()
    }

    /// A control channel's `reports` message: kept, answered, and the newest new one told about.
    static func take(_ msg: [String: Any], from receiver: Receiver, on ctl: ControlChannel, base: URL) {
        let items = msg["items"] as? [[String: Any]] ?? []
        let (ids, new) = store(model: receiver.model, items: items, base: base)
        if !ids.isEmpty { ctl.send(["t": "reports_taken", "ids": ids]) }
        guard new > 0 else { return }
        log("\(receiver.name): \(new) new report(s), see `luchctl reports`")
        if let last = list(base: base).last(where: { ids.contains($0["id"] as? String ?? "") }) {
            onNew?(last, receiver.name)
        }
    }
}
