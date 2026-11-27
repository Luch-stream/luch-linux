// `luchctl` started by a browser for the Luch extension (Chrome passes the extension's origin, Firefox the manifest
// path and the extension's id): native messaging over stdin/stdout, a 4-byte native-endian length and JSON, the
// same requests and answers as luch-linux's luchctl/src/native.rs, here passed to Luch.app's local socket.
// SPDX-License-Identifier: GPL-3.0-or-later
import Foundation
import LuchLocal

struct HostError: Error {
    let text: String
}

enum NativeHost {
    static let chromeID = "enoggfebgncdlpimolmheljkneecempd"
    static let firefoxID = "luch@toxblh.github.io"

    static func startedByBrowser(_ args: [String]) -> Bool {
        (args.first?.hasPrefix("chrome-extension://") ?? false) || (args.count > 1 && args[1] == firefoxID)
    }

    static func readFrame(_ h: FileHandle) -> [String: Any]? {
        let len = h.readData(ofLength: 4)
        guard len.count == 4 else { return nil }
        let n = len.withUnsafeBytes { $0.load(as: UInt32.self) }
        guard n <= 1024 * 1024 else { return nil }
        let body = h.readData(ofLength: Int(n))
        return (try? JSONSerialization.jsonObject(with: body)) as? [String: Any]
    }

    static func frame(_ obj: [String: Any]) -> Data {
        let body = (try? JSONSerialization.data(withJSONObject: obj)) ?? Data("{}".utf8)
        var n = UInt32(body.count)
        var out = Data(bytes: &n, count: 4)
        out.append(body)
        return out
    }

    private static let out = FileHandle.standardOutput
    private static let lock = NSLock()

    static func write(_ obj: [String: Any]) {
        lock.lock()
        out.write(frame(obj))
        lock.unlock()
    }

    static func local(_ req: [String: Any], timeout: TimeInterval = 30) -> [String: Any] {
        (try? LocalClient.request(req, timeout: timeout)) ?? ["error": "Luch.app is not running"]
    }

    /// One request from the extension; the answer's `ok` value, or the reason it failed.
    static func handle(_ req: [String: Any]) -> Result<Any, HostError> {
        let receiver = req["receiver"] as? String
        func pass(_ r: [String: Any], _ value: (([String: Any]) -> Any)) -> Result<Any, HostError> {
            if let e = r["error"] as? String { return .failure(HostError(text: e)) }
            return .success(value(r))
        }
        switch req["op"] as? String ?? "" {
        case "version":
            return .success("0.4.0 (macos)")
        case "receivers":
            return pass(local(["cmd": "receivers"])) { r in
                (r["receivers"] as? [[String: Any]] ?? []).map {
                    ["id": $0["id"] ?? "", "name": $0["name"] ?? "", "model": $0["model"] ?? "",
                     "connected": false, "mirroring": $0["mirroring"] ?? false, "media": NSNull()]
                }
            }
        case "cast":
            // The page may only cast links, never this Mac's files.
            let items = (req["items"] as? [String] ?? []).filter { $0.hasPrefix("http://") || $0.hasPrefix("https://") }
            guard !items.isEmpty else { return .failure(HostError(text: "nothing to cast")) }
            watchLock.lock()
            watched.insert(receiver ?? "")
            watchLock.unlock()
            return pass(local(["cmd": "cast", "id": receiver ?? "", "items": items,
                               "options": req["options"] as? [String: Any] ?? [:]])) { $0["kind"] ?? "media" }
        case "cmd":
            guard let cmd = req["cmd"] as? [String: Any] else { return .failure(HostError(text: "no cmd")) }
            return pass(local(["cmd": "media_cmd", "id": receiver ?? "", "media": cmd])) { _ in true }
        case "state":
            return pass(local(["cmd": "media_state", "id": receiver ?? ""])) { $0["state"] ?? [:] }
        case "stop":
            return pass(local(["cmd": "stop_cast", "id": receiver ?? ""])) { _ in true }
        case let op:
            return .failure(HostError(text: "unknown op \(op)"))
        }
    }

    /// Receivers something was cast to: their state is followed (the local socket answers, it does not call back).
    private static var watched: Set<String> = []
    private static let watchLock = NSLock()

    static func run() -> Never {
        // State changes, once a second, as events.
        Thread.detachNewThread {
            var last: [String: String] = [:]
            while true {
                Thread.sleep(forTimeInterval: 1)
                watchLock.lock()
                let ids = watched
                watchLock.unlock()
                for id in ids {
                    let r = local(["cmd": "media_state", "id": id], timeout: 3)
                    let state = r["state"] as? [String: Any] ?? [:]
                    var key = state
                    key.removeValue(forKey: "pos") // moves on by itself: not news
                    let text = String(decoding: (try? JSONSerialization.data(withJSONObject: key, options: .sortedKeys)) ?? Data(), as: UTF8.self)
                    if text != last[id] {
                        let wasPlaying = last[id].map { $0 != "{}" } ?? false
                        last[id] = text
                        if !state.isEmpty {
                            write(["event": "state", "receiver": id, "state": state])
                        } else if wasPlaying {
                            write(["event": "state", "receiver": id, "state": ["state": "stopped"]])
                        }
                    }
                }
            }
        }
        let input = FileHandle.standardInput
        while let req = readFrame(input) {
            DispatchQueue.global().async {
                let id = req["id"] ?? NSNull()
                switch handle(req) {
                case .success(let v): write(["id": id, "ok": v])
                case .failure(let e): write(["id": id, "error": e.text])
                }
            }
        }
        exit(0) // the extension let go of us
    }

    // MARK: - installing

    static func manifest(path: String, firefox: Bool) -> [String: Any] {
        var m: [String: Any] = ["name": "dev.luch.host", "description": "Luch: cast a video from a web page to a TV",
                                "path": path, "type": "stdio"]
        if firefox { m["allowed_extensions"] = [firefoxID] } else { m["allowed_origins"] = ["chrome-extension://\(chromeID)/"] }
        return m
    }

    /// Where each browser on macOS looks for host manifests (under ~/Library/Application Support).
    static let dirs: [(String, Bool)] = [
        ("Google/Chrome/NativeMessagingHosts", false),
        ("Chromium/NativeMessagingHosts", false),
        ("BraveSoftware/Brave-Browser/NativeMessagingHosts", false),
        ("Microsoft Edge/NativeMessagingHosts", false),
        ("Yandex/YandexBrowser/NativeMessagingHosts", false),
        ("Arc/User Data/NativeMessagingHosts", false),
        ("Mozilla/NativeMessagingHosts", true),
    ]

    /// Writes the manifest for every browser whose data folder exists; returns the files.
    static func install(exe: String, support: URL) throws -> [String] {
        var done: [String] = []
        for (dir, firefox) in dirs {
            let target = support.appendingPathComponent(dir, isDirectory: true)
            guard FileManager.default.fileExists(atPath: target.deletingLastPathComponent().path) else { continue }
            try FileManager.default.createDirectory(at: target, withIntermediateDirectories: true)
            let file = target.appendingPathComponent("dev.luch.host.json")
            let data = try JSONSerialization.data(withJSONObject: manifest(path: exe, firefox: firefox),
                                                  options: [.prettyPrinted, .sortedKeys])
            try data.write(to: file)
            done.append(file.path)
        }
        return done
    }
}
