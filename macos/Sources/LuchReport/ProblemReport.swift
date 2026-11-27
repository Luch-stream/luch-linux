// "Report a Problem…": one file with everything to look at, for someone who knows nothing about the insides. This
// Mac's log, the logs of the receivers it can ask without anything showing on a TV (paired ones that list "logs"),
// about.txt (versions, this Mac, each receiver's welcome, the last session's plan and stats, what the person wrote)
// and the receivers' crash reports kept here, zipped into ~/Downloads. Nothing is sent anywhere: the person takes
// the file where they like (a chat, an issue). As luchd's report.
// SPDX-License-Identifier: GPL-3.0-or-later
import Darwin
import Foundation
import LuchNet
import LuchProto

public enum ProblemReport {
    /// This Mac's log taken, and asked of each receiver.
    public static let logKB = 1024

    public struct Input {
        public var what: String
        /// "Hide addresses and names".
        public var hide: Bool
        /// The receivers with the Luch app that are on now, the ones in use first.
        public var receivers: [Receiver]
        /// The ids of those in use now (mirroring, casting).
        public var inUse: Set<String>
        /// Receivers' names met before, not on now: hidden as well.
        public var otherNames: [String]
        /// This Mac in the hellos.
        public var me: (id: String, name: String)
        /// "Luch 1.2 (luch/0.1 (macos))": the app's version.
        public var app: String
        /// "1.2": in a sent report's release.
        public var version = "?"
        public var logPath: String?
        public var downloads: URL
        public var now = Date()
        /// Where the receivers' crash reports are kept (Reports.swift).
        public var reportsBase: URL = supportDirectory()
        public var connector: Connector = .shared
        public var machine: Machine = .current()

        public init(what: String, hide: Bool, receivers: [Receiver], inUse: Set<String>, otherNames: [String],
                    me: (id: String, name: String), app: String, logPath: String?, downloads: URL) {
            self.what = what
            self.hide = hide
            self.receivers = receivers
            self.inUse = inUse
            self.otherNames = otherNames
            self.me = me
            self.app = app
            self.logPath = logPath
            self.downloads = downloads
        }
    }

    public struct Result {
        public let file: URL
        /// A few lines for a chat: what happened, versions, devices.
        public let description: String
        /// A new issue with its title and body filled in, saying to attach the file.
        public let issueURL: String
        /// One line per receiver: its log taken, or why not.
        public let notes: [String]
        /// What a sent report carries (ReportSender), as hidden as the file.
        public let facts: Facts
    }

    /// The file's texts and what it is about, for "Send to the Developers".
    public struct Facts {
        public var what: String
        /// (name in the zip, text), about.txt first.
        public var texts: [(String, String)]
        public var app: String
        public var version: String
        public var os: String
        public var model: String
        /// The receivers' models.
        public var receivers: [String]
        /// The codec of the last session ("HEVC"), from its plan line; empty when there was none.
        public var codec: String
    }

    /// This Mac: what it is and the names that tell whose it is.
    public struct Machine {
        public var os: String
        public var model: String
        public var names: [String]
        public var user: [String]
        public var home: String

        public static func current() -> Machine {
            let p = ProcessInfo.processInfo
            let local = Host.current().localizedName ?? ""
            let host = p.hostName
            let short = host.split(separator: ".").first.map(String.init) ?? host
            let full = NSFullUserName()
            // The words of the person's full name too: "Ann's MacBook" and a log line with "Ann" in it.
            let words = full.split(whereSeparator: { !$0.isLetter }).map(String.init).filter { $0.count >= 3 }
            let mem = sysctlInt("hw.memsize").map { " \($0 >> 30) GB" } ?? ""
            return Machine(os: "macOS \(p.operatingSystemVersionString)",
                           model: [sysctlString("hw.model"), sysctlString("machdep.cpu.brand_string")]
                               .compactMap { $0 }.joined(separator: ", ") + mem,
                           names: [local, host, short].filter { !$0.isEmpty },
                           user: [NSUserName(), full] + words, home: NSHomeDirectory())
        }
    }

    /// What one receiver gave.
    struct Part {
        let receiver: Receiver
        var welcome: [String: Any]?
        var log: String?
        var note: String
    }

    /// Collects, writes and zips the report; the receivers are asked all at once, `ReceiverLogs.timeout` each.
    public static func make(_ input: Input) throws -> Result {
        let parts = collect(input)
        let red = Redactor()
        if input.hide {
            input.machine.names.forEach { red.hide($0, as: "Computer") }
            red.hide(input.me.name, as: "Computer")
            red.hide(input.machine.home, as: "~")
            input.machine.user.forEach { red.hide($0, as: "User") }
            for p in parts {
                red.receiver([p.receiver.name] + [p.welcome?["name"] as? String].compactMap { $0 })
            }
            input.otherNames.forEach { red.receiver([$0]) }
        }
        let text: (String) -> String = { input.hide ? red.redact($0) : $0 }

        let macLog = input.logPath.map { RotatingLog.tail($0, maxBytes: logKB * 1024) } ?? ""
        let base = uniqueBase(in: input.downloads, now: input.now)
        let work = FileManager.default.temporaryDirectory.appendingPathComponent("luch-report-\(UUID().uuidString)")
        let dir = work.appendingPathComponent(base, isDirectory: true)
        try FileManager.default.createDirectory(at: dir, withIntermediateDirectories: true)
        defer { try? FileManager.default.removeItem(at: work) }

        var texts: [(String, String)] = []
        func write(_ name: String, _ s: String) throws {
            let t = text(s)
            texts.append((name, t))
            try Data(t.utf8).write(to: dir.appendingPathComponent(name))
        }
        try write("luch-mac.log", macLog)
        for (i, p) in parts.enumerated() {
            if let l = p.log { try write("receiver-\(i + 1).log", l) }
        }
        let crashes = Reports.list(base: input.reportsBase)
        if !crashes.isEmpty, let d = try? JSONSerialization.data(withJSONObject: crashes,
                                                                 options: [.prettyPrinted, .sortedKeys, .withoutEscapingSlashes]) {
            try write("receiver-reports.json", String(decoding: d, as: UTF8.self))
        }
        try write("about.txt", about(input, parts: parts, crashes: crashes.count, macLog: macLog))

        let file = input.downloads.appendingPathComponent(base + ".zip")
        try zip(dir, to: file)
        let notes = parts.map { text("\($0.receiver.name): \($0.note)") }
        let description = text(describe(input, parts: parts, file: file.lastPathComponent))
        let what = text(input.what)
        let plan = lastSession(macLog).first { $0.hasPrefix("plan: ") }
        let facts = Facts(what: what, texts: texts.suffix(1) + texts.dropLast(), app: input.app, version: input.version,
                          os: input.machine.os, model: input.machine.model,
                          receivers: parts.map(\.receiver.model).filter { !$0.isEmpty },
                          codec: plan.flatMap { $0.dropFirst(6).split(separator: " ").first.map(String.init) } ?? "")
        return Result(file: file, description: description,
                      issueURL: issueURL(what: what, description: description, file: file.lastPathComponent),
                      notes: notes, facts: facts)
    }

    // MARK: - receivers

    static func collect(_ input: Input) -> [Part] {
        let rs = input.receivers
        var parts = [Part?](repeating: nil, count: rs.count)
        let lock = NSLock()
        DispatchQueue.concurrentPerform(iterations: rs.count) { i in
            let p = ask(rs[i], input)
            lock.lock()
            parts[i] = p
            lock.unlock()
        }
        return parts.compactMap { $0 }
    }

    /// One receiver's welcome and log, or why not. Only a paired receiver is asked: an unpaired one would show a
    /// code (or Allow) on its TV, which a report must never do.
    static func ask(_ r: Receiver, _ input: Input) -> Part {
        var part = Part(receiver: r, note: "")
        guard r.tlsPort > 0, input.connector.isPaired(r.id) else {
            part.note = "not paired with this Mac: its log is not asked for (that would ask on the TV)"
            return part
        }
        let c: Connection
        do {
            c = try input.connector.open(r, hello: ["t": "hello", "v": Int(Proto.version), "id": input.me.id,
                                                    "name": input.me.name, "app": Proto.app,
                                                    "want": [ReceiverLogs.mode, "reports"]],
                                         mayPair: false)
        } catch ConnectError.pairingNeeded {
            part.note = "asks for pairing again (it forgot this Mac): its log is not asked for"
            return part
        } catch {
            part.note = "did not answer: \(error.localizedDescription)"
            return part
        }
        defer { c.channel.bye() }
        part.welcome = c.welcome
        guard (c.welcome["modes"] as? [String])?.contains(ReceiverLogs.mode) == true else {
            part.note = "its Luch cannot hand over its log yet (update it)"
            return part
        }
        do {
            let l = try ReceiverLogs.fetch(c.channel, maxKB: logKB)
            part.log = l
            part.note = "log taken (\(max(1, l.utf8.count / 1024)) KB)"
        } catch {
            part.note = error.localizedDescription
        }
        return part
    }

    // MARK: - texts

    static func about(_ input: Input, parts: [Part], crashes: Int, macLog: String) -> String {
        var s = "Luch problem report, \(stamp(input.now, "yyyy-MM-dd HH:mm ZZZZZ"))\n\n"
        s += "What happened:\n\(input.what.isEmpty ? "(nothing written)" : input.what)\n\n"
        s += "This computer\n  \(input.app)\n  \(input.machine.os)\n  \(input.machine.model)\n"
        s += "  addresses and names \(input.hide ? "hidden" : "kept as they are")\n\n"
        let session = lastSession(macLog)
        s += "Last session (from luch-mac.log)\n"
        s += session.isEmpty ? "  none in the log\n" : session.map { "  \($0)\n" }.joined()
        s += "\nReceivers\n"
        if parts.isEmpty { s += "  none on\n" }
        for (i, p) in parts.enumerated() {
            let r = p.receiver
            s += "\(i + 1). \(r.name)\(input.inUse.contains(r.id) ? " (in use)" : ""): \(p.note)\n"
            s += "   model \(r.model.isEmpty ? "?" : r.model), at \(r.address) via \(r.interfaceName.isEmpty ? "?" : r.interfaceName)"
            s += r.tlsPort > 0 ? ", TLS" : ", no TLS"
            s += input.connector.isPaired(r.id) ? ", paired\n" : ", not paired\n"
            if p.log != nil { s += "   log: receiver-\(i + 1).log\n" }
            if let w = p.welcome { s += welcomeLines(w) }
        }
        s += "\nCrash reports kept: \(crashes)\(crashes > 0 ? " (receiver-reports.json)" : "")\n"
        return s
    }

    /// What a welcome says about the receiver: what it is and can do (its app list and volume are left out).
    static func welcomeLines(_ w: [String: Any]) -> String {
        var s = "   Android SDK \(json(w["sdk"])), decoders \(json(w["decoders"])), max \(json(w["max"]))\n"
        s += "   modes \(json(w["modes"]))\n"
        for key in ["hw", "bench", "limits", "sync", "input"] where w[key] != nil && !(w[key] is NSNull) {
            s += "   \(key): \(json(w[key]))\n"
        }
        return s
    }

    /// The session lines the engine logs: whom it connected to, the receiver's caps, the plan, the last stats.
    static func lastSession(_ log: String) -> [String] {
        let lines = log.split(separator: "\n")
        return ["] connected to ", "] receiver: ", "] plan: ", "] stats: "].compactMap { key in
            lines.last(where: { $0.contains(key) }).map { l in
                let s = String(l)
                return s.range(of: key).map { String(s[s.index($0.lowerBound, offsetBy: 2)...]) } ?? s
            }
        }
    }

    static func describe(_ input: Input, parts: [Part], file: String) -> String {
        var s = "Luch problem report (\(file))\n"
        let what = input.what.trimmingCharacters(in: .whitespacesAndNewlines)
        s += "What happened: \(what.isEmpty ? "-" : String(what.prefix(500)))\n"
        s += "This computer: \(input.app), \(input.machine.os), \(input.machine.model)\n"
        for p in parts {
            let sdk = (p.welcome?["sdk"] as? NSNumber).map { ", Android SDK \($0)" } ?? ""
            s += "\(p.receiver.name): \(p.receiver.model.isEmpty ? "?" : p.receiver.model)\(sdk), \(p.note)\n"
        }
        return s
    }

    static func issueURL(what: String, description: String, file: String) -> String {
        let line = what.split(separator: "\n").first.map { String($0.prefix(80)) } ?? ""
        let title = line.isEmpty ? "Problem report from Luch for macOS" : "Problem: \(line)"
        var body = description
        while true {
            let full = body + "\nThe file \(file) has the logs: please attach it here (drag it into this box).\n"
            let url = "\(Reports.issues)?title=\(Reports.encode(title))&body=\(Reports.encode(full))"
            if url.utf8.count <= 7000 || body.isEmpty { return url }
            body = String(body.prefix(body.count * 3 / 4)) + "…"
        }
    }

    private static func json(_ v: Any?) -> String {
        guard let v, !(v is NSNull) else { return "?" }
        if let s = v as? String { return s }
        if let d = try? JSONSerialization.data(withJSONObject: v, options: [.sortedKeys, .fragmentsAllowed, .withoutEscapingSlashes]) {
            return String(decoding: d, as: UTF8.self)
        }
        return "\(v)"
    }

    // MARK: - the file

    /// `Luch-report-YYYY-MM-DD-HHMM`, with -2, -3… when that one is taken.
    static func uniqueBase(in dir: URL, now: Date) -> String {
        let base = "Luch-report-" + stamp(now, "yyyy-MM-dd-HHmm")
        var name = base
        var n = 2
        while FileManager.default.fileExists(atPath: dir.appendingPathComponent(name + ".zip").path) {
            name = "\(base)-\(n)"
            n += 1
        }
        return name
    }

    /// A zip every system opens (no resource forks), its files in one folder of the zip's name.
    static func zip(_ dir: URL, to file: URL) throws {
        let p = Process()
        p.executableURL = URL(fileURLWithPath: "/usr/bin/ditto")
        p.arguments = ["-c", "-k", "--sequesterRsrc", "--keepParent", dir.path, file.path]
        let err = Pipe()
        p.standardError = err
        try p.run()
        p.waitUntilExit()
        guard p.terminationStatus == 0 else {
            let why = String(decoding: err.fileHandleForReading.readDataToEndOfFile(), as: UTF8.self)
            throw ReportError("cannot write \(file.lastPathComponent): \(why.trimmingCharacters(in: .whitespacesAndNewlines))")
        }
    }

    private static func stamp(_ d: Date, _ format: String) -> String {
        let f = DateFormatter()
        f.locale = Locale(identifier: "en_US_POSIX")
        f.dateFormat = format
        return f.string(from: d)
    }
}

public struct ReportError: Error, LocalizedError {
    public let errorDescription: String?
    public init(_ why: String) { errorDescription = why }
}

private func sysctlString(_ name: String) -> String? {
    var size = 0
    guard sysctlbyname(name, nil, &size, nil, 0) == 0, size > 0 else { return nil }
    var buf = [CChar](repeating: 0, count: size)
    guard sysctlbyname(name, &buf, &size, nil, 0) == 0 else { return nil }
    return String(cString: buf)
}

private func sysctlInt(_ name: String) -> Int64? {
    var v: Int64 = 0
    var size = MemoryLayout<Int64>.size
    return sysctlbyname(name, &v, &size, nil, 0) == 0 ? v : nil
}
