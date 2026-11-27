// luchctl's find, add, refresh, id, known, wake and info: the Linux luchctl's commands and lines, over the app's
// local socket.
// SPDX-License-Identifier: GPL-3.0-or-later
import Foundation
import LuchLocal

enum FindCommands {
    static let words: Set<String> = ["find", "add", "refresh", "id", "known", "wake", "info"]

    static func handles(_ args: [String]) -> Bool { words.contains(args.first ?? "") }

    /// Runs the command; the exit code.
    static func run(_ args: [String]) -> Int32 {
        do {
            try dispatch(Array(args.dropFirst()), args[0])
            return 0
        } catch {
            print("error: \(error.localizedDescription)")
            return 1
        }
    }

    private static func ask(_ request: [String: Any], timeout: TimeInterval = 10) throws -> [String: Any] {
        let r: [String: Any]
        do {
            r = try LocalClient.request(request, timeout: timeout)
        } catch {
            throw Refusal(error.localizedDescription)
        }
        if let e = r["error"] as? String { throw Refusal(e) }
        return r
    }

    private static func dispatch(_ rest: [String], _ word: String) throws {
        switch (word, rest.count) {
        case ("find", 0...1): try find(rest.first ?? "all")
        case ("add", 1): print("added \(try ask(["cmd": "add", "link": rest[0]])["id"] as? String ?? "?")")
        case ("refresh", 0): _ = try ask(["cmd": "refresh"])
        case ("id", 0): print(try ask(["cmd": "sender_id"])["id"] as? String ?? "")
        case ("known", 0): FindText.known(try knownRows()).forEach { print($0) }
        case ("info", 0...1): try info(rest.first)
        case ("wake", 0...1): try wake(rest.first)
        case ("wake", 2) where rest[1] == "learn": try learn(rest[0])
        case ("wake", 3) where ["mac", "ble"].contains(rest[1]):
            let r = try FindText.pickKnown(try knownRows(), rest[0])
            _ = try ask(["cmd": "set_wake", "id": r["id"] as? String ?? "", "kind": rest[1],
                         "value": rest[2] == "off" ? "" : rest[2]])
        default:
            throw Refusal("usage: luchctl find [auto|all|mdns,scan,ble,sound,qr,direct] | add <link|host[:port]> | refresh | id"
                + " | info [r] | known | wake [r] | wake <r> learn | wake <r> mac|ble <value>|off")
        }
    }

    private static func knownRows() throws -> [[String: Any]] {
        try ask(["cmd": "known"], timeout: 15)["known"] as? [[String: Any]] ?? []
    }

    /// Follows the climb until it ends, saying what starts and ends, then lists what is there. A climb already under
    /// way (refresh starts the automatic one) is followed; ours comes after it when it found nothing.
    private static func find(_ mode: String) throws {
        var last: [String: Any] = ["running": true]
        let status = try ask(["cmd": "find_status"])["status"] as? [String: Any] ?? [:]
        var ours = status["running"] as? Bool != true
        if ours {
            _ = try ask(["cmd": "find", "mode": mode])
        } else {
            print("already looking (\(status["mode"] as? String ?? ""))")
            last = status
        }
        while true {
            Thread.sleep(forTimeInterval: 0.3)
            let now = try ask(["cmd": "find_status"])["status"] as? [String: Any] ?? [:]
            FindText.progressLines(before: last, now: now).forEach { print($0) }
            last = now
            guard now["running"] as? Bool == false else { continue }
            if !ours, (now["found"] as? NSNumber)?.intValue ?? 0 == 0 {
                ours = true
                last = ["running": true]
                _ = try ask(["cmd": "find", "mode": mode])
                continue
            }
            break
        }
        let list = try ask(["cmd": "receivers"])["receivers"] as? [[String: Any]] ?? []
        let via = try ask(["cmd": "discovered"])["discovered"] as? [String: [String: Any]] ?? [:]
        if list.isEmpty { print("(no receivers: `luchctl find` tries harder)") }
        for r in list {
            let id = r["id"] as? String ?? ""
            var how = (via[id]?["via"] as? [String] ?? []).joined(separator: "+")
            if let rssi = via[id]?["rssi"] as? NSNumber { how += " \(rssi) dBm" }
            print([r["name"] as? String ?? "?", "\(r["address"] as? String ?? "?"):\(r["port"] as? NSNumber ?? 0)",
                   r["iface"] as? String ?? "-", r["model"] as? String ?? "", how.isEmpty ? "-" : how]
                .joined(separator: "\t"))
        }
        // Heard over Bluetooth but not on this network: reachable only by connecting directly.
        let nearby = (try? ask(["cmd": "direct_status"])["status"] as? [String: Any])??["nearby"] as? [[String: Any]] ?? []
        for n in nearby where n["listed"] as? Bool != true {
            let id = n["id"] as? String ?? "?"
            let rssi = (n["rssi"] as? NSNumber).map { " \($0) dBm" } ?? ""
            print("\(n["name"] as? String ?? id)\t-\t-\t\tble\(rssi), direct possible: `luchctl direct \(id)`")
        }
    }

    private static func info(_ key: String?) throws {
        var q: [String: Any] = ["cmd": "info"]
        if let key { q["id"] = key }
        let v = try ask(q, timeout: 10)["info"] as? [String: Any] ?? [:]
        let android = v["android"] as? String ?? ""
        print(android.isEmpty ? "Android version unknown until connected (luchctl connect)" : android)
        let lag = (v["audio_latency_ms"] as? NSNumber).map { ", trails the picture by about \($0) ms" } ?? ""
        print("sound: \(v["audio"] as? String ?? "auto")\(lag)")
        (v["limits"] as? [String] ?? []).forEach { print("  - \($0)") }
        if v["paired"] as? Bool == true {
            print(v["encrypted"] as? Bool == true ? "paired: encrypted (TLS, sealed media)"
                                                  : "paired, but it offers no encryption now: refused (`luchctl forget`)")
        } else {
            print(v["tls"] as? Bool == true ? "not paired yet: the first connection asks for the code on the TV"
                                            : "older receiver: no pairing, not encrypted")
        }
    }

    private static func wake(_ key: String?) throws {
        let r = try FindText.pickKnown(try knownRows(), key)
        let (id, name) = (r["id"] as? String ?? "", r["name"] as? String ?? "")
        if r["online"] as? Bool == true {
            print("\(name) is already on")
            return
        }
        let how = try ask(["cmd": "wake", "id": id], timeout: 30)["how"] as? String ?? ""
        print("waking \(name) (\(how)), waiting for it on the LAN…")
        let deadline = Date().addingTimeInterval(90)
        while Date() < deadline {
            if try knownRows().contains(where: { $0["id"] as? String == id && $0["online"] as? Bool == true }) {
                print("\(name) is on")
                return
            }
            _ = try? ask(["cmd": "refresh"])
            Thread.sleep(forTimeInterval: 5)
        }
        throw Refusal("\(name) did not show up in 90 s")
    }

    private static func learn(_ key: String) throws {
        let r = try FindText.pickKnown(try knownRows(), key)
        print("listening 30 s: with \(r["name"] as? String ?? key) off, press the power key on its remote near this computer…")
        let adv = try ask(["cmd": "learn_wake", "id": r["id"] as? String ?? "", "seconds": 30], timeout: 45)["ble"] as? String ?? ""
        print("learned \(adv); `luchctl wake \(key)` turns it on now")
    }
}
