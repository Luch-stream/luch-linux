// A control channel of its own to one receiver for what is cast to it (PROTOCOL.md 4b-4g), next to a mirroring
// session's: the same hello, then media, photos, documents, YouTube, volume and sleep, and the receiver's reports.
// SPDX-License-Identifier: GPL-3.0-or-later
import Foundation
import LuchEngine
import LuchNet
import LuchProto

public final class CastSession {
    public let receiver: Receiver
    public let welcome: [String: Any]
    let ctl: ControlChannel
    private let lock = NSLock()
    private var lastState: [String: Any] = [:]
    private var docPages: (page: Int, pages: Int) = (0, 0)
    private var stopped = false
    /// Every media_state (and {"state":"stopped"} at the end), for whoever follows it (luchctl, the browser).
    public var onState: (([String: Any]) -> Void)?

    public var isOpen: Bool { !ctl.isClosed }

    private init(receiver: Receiver, ctl: ControlChannel, welcome: [String: Any]) {
        self.receiver = receiver
        self.ctl = ctl
        self.welcome = welcome
    }

    public static func open(_ r: Receiver) throws -> CastSession {
        guard let ctl = ControlChannel.open(ip: r.address, port: r.port, interfaceIndex: r.interfaceIndex) else {
            throw CastError.bad("cannot reach \(r.name) (\(r.address):\(r.port))")
        }
        let me = senderIdentity()
        ctl.send(["t": "hello", "v": Int(Proto.version), "id": me.id, "name": me.name, "app": Proto.app,
                  "want": ["media", "photo"]])
        guard let welcome = ctl.waitFor("welcome", timeout: 5) else {
            ctl.closeChannel("no welcome")
            throw CastError.bad("\(r.name) did not answer")
        }
        let s = CastSession(receiver: r, ctl: ctl, welcome: welcome)
        ctl.onEvent = { [weak s] msg in s?.event(msg) }
        ctl.onClosed = { [weak s] _ in s?.ended("closed") }
        return s
    }

    public func can(_ mode: String) -> Bool {
        (welcome["modes"] as? [String])?.contains(mode) ?? false
    }

    private func event(_ m: [String: Any]) {
        switch m["t"] as? String ?? "" {
        case "media_state":
            lock.lock()
            let ignore = stopped
            if !ignore { lastState = m }
            lock.unlock()
            if !ignore { onState?(m) }
        case "media_ready":
            lock.lock()
            stopped = false
            lock.unlock()
        case "doc_ready":
            lock.lock()
            docPages = (0, (m["pages"] as? NSNumber)?.intValue ?? 0)
            lock.unlock()
        case "doc_page":
            lock.lock()
            docPages.page = (m["page"] as? NSNumber)?.intValue ?? 0
            lock.unlock()
        case "stop":
            ended((m["reason"] as? String) ?? "receiver")
        default:
            break
        }
    }

    private func ended(_ reason: String) {
        lock.lock()
        let had = !lastState.isEmpty
        lastState = [:]
        lock.unlock()
        if had { onState?(["t": "media_state", "state": "stopped", "reason": reason]) }
    }

    public var state: [String: Any] {
        lock.lock()
        defer { lock.unlock() }
        return lastState
    }

    /// Sends what `items` make, and waits for the receiver to take it. Returns "media", "photo", "doc" or "app".
    public func cast(_ items: [String], options: CastOptions, server: FileServer) throws -> String {
        var items = items
        for (i, s) in items.enumerated() where Cast.kind(s) == .document && Cast.ext(s) != "pdf" {
            let out = FileManager.default.temporaryDirectory.appendingPathComponent("luch-doc", isDirectory: true)
            items[i] = try Cast.toPDF(try Cast.fileURL(s), outDir: out).path
        }
        let msg = try Cast.build(items, o: options, server: server, client: receiver.address, me: ctl.localAddress)
        let t = msg["t"] as? String ?? ""
        ctl.drain()
        lock.lock()
        stopped = false
        lock.unlock()
        ctl.send(msg)
        guard let r = ctl.waitForAny(["media_ready", "photo_ready", "doc_ready", "app_opened", "error"], timeout: 10) else {
            throw CastError.bad("\(receiver.name) did not answer")
        }
        if r["t"] as? String == "error" {
            let code = r["code"] as? String ?? "error"
            if code == "unknown" { throw CastError.bad("this receiver version cannot show that yet: update it") }
            if code == "no_app" { throw CastError.bad("the TV has no YouTube app") }
            throw CastError.bad(code + ((r["detail"] as? String).map { ": \($0)" } ?? ""))
        }
        return t == "open_app" ? "app" : t
    }

    public func command(_ cmd: [String: Any]) {
        var m = cmd
        m["t"] = "media_cmd"
        ctl.send(m)
    }

    public func stop() {
        lock.lock()
        stopped = true
        lock.unlock()
        ctl.send(["t": "stop"])
        ended("sender")
    }

    public func sleep() throws {
        ctl.drain()
        ctl.send(["t": "sleep"])
        guard let r = ctl.waitForAny(["sleeping", "error"], timeout: 5) else { return } // the TV went with its network
        if r["t"] as? String == "error" {
            switch r["sleep"] as? String {
            case "off": throw CastError.bad("sleep is off on the receiver: turn on \"Luch\" in its accessibility settings")
            case "no": throw CastError.bad("the receiver's Android is too old to be put to sleep (9 or newer)")
            default: throw CastError.bad("this receiver version cannot sleep yet: update it")
            }
        }
    }

    /// "0".."100", "+", "-", "mute", "unmute"; the receiver's volume after it.
    public func volume(_ what: String) throws -> [String: Any] {
        var m: [String: Any] = ["t": "volume"]
        switch what {
        case "+", "up": m["step"] = 1
        case "-", "down": m["step"] = -1
        case "mute": m["muted"] = true
        case "unmute": m["muted"] = false
        default:
            guard let n = Int(what.replacingOccurrences(of: "%", with: "")), (0...100).contains(n) else {
                throw CastError.bad("volume: 0..100, +, -, mute or unmute")
            }
            m["level"] = n
        }
        ctl.drain()
        ctl.send(m)
        guard let r = ctl.waitForAny(["volume", "error"], timeout: 3) else { throw CastError.bad("no answer") }
        if r["t"] as? String == "error" { throw CastError.bad("the TV keeps its volume to itself") }
        return r.filter { $0.key != "t" }
    }

    /// "next", "prev" or a page from 1; returns "page/pages".
    public func page(_ what: String) throws -> String {
        lock.lock()
        let (page, pages) = docPages
        lock.unlock()
        guard pages > 0 else { throw CastError.bad("no document on that receiver") }
        let to: Int
        switch what {
        case "next", "+": to = min(page + 1, pages - 1)
        case "prev", "-": to = max(page - 1, 0)
        default:
            guard let n = Int(what), (1...pages).contains(n) else { throw CastError.bad("page 1..\(pages), next or prev") }
            to = n - 1
        }
        ctl.send(["t": "doc_page", "page": to])
        return "\(to + 1)/\(pages)"
    }

    public func close() {
        ctl.bye()
    }
}
