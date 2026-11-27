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
    /// What is shown changed: something started or ended, a page turned, the video paused (the menu follows it).
    public var onShowing: (() -> Void)?
    private var shown: String?
    private var volumeNow: [String: Any]?
    /// Every message from the receiver, and {"t":"stop"} when what it shows for us ends (the presenter view).
    public var onMessage: (([String: Any]) -> Void)?

    /// The receiver's volume as it last said ({"level":N,"muted":B}); nil when it keeps its volume to itself.
    public var currentVolume: [String: Any]? {
        lock.lock()
        defer { lock.unlock() }
        return volumeNow
    }

    /// What this session shows on the receiver now: "media", "photo", "doc" or "app"; nil when nothing.
    public var showing: String? {
        lock.lock()
        defer { lock.unlock() }
        return shown
    }

    /// The document's page (from 0) and page count; (0, 0) without one.
    public var pages: (page: Int, pages: Int) {
        lock.lock()
        defer { lock.unlock() }
        return docPages
    }

    public var isOpen: Bool { !ctl.isClosed }
    /// TLS (v1), not the plain port of an older receiver.
    public var isEncrypted: Bool { ctl.secure }

    private init(receiver: Receiver, ctl: ControlChannel, welcome: [String: Any]) {
        self.receiver = receiver
        self.ctl = ctl
        self.welcome = welcome
    }

    public static func open(_ r: Receiver) throws -> CastSession {
        let me = senderIdentity()
        // TLS and pairing when the receiver offers them; fails with "asks for pairing" while a code waits.
        let c = try Connector.shared.open(r, hello: ["t": "hello", "v": Int(Proto.version), "id": me.id, "name": me.name,
                                                     "app": Proto.app, "want": ["media", "photo", "reports"]])
        let ctl = c.channel
        let welcome = c.welcome
        let s = CastSession(receiver: r, ctl: ctl, welcome: welcome)
        s.volumeNow = welcome["volume"] as? [String: Any]
        ctl.onEvent = { [weak s] msg in s?.event(msg) }
        ctl.onClosed = { [weak s] _ in s?.ended("closed") }
        return s
    }

    public func can(_ mode: String) -> Bool {
        (welcome["modes"] as? [String])?.contains(mode) ?? false
    }

    private func event(_ m: [String: Any]) {
        if m["t"] as? String != "stop" { onMessage?(m) }
        switch m["t"] as? String ?? "" {
        case "media_state":
            lock.lock()
            let ignore = stopped
            if !ignore { lastState = m }
            if !ignore, m["state"] as? String == "stopped", shown == "media" { shown = nil }
            lock.unlock()
            if !ignore {
                onState?(m)
                onShowing?()
            }
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
            onShowing?()
        case "volume":
            lock.lock()
            volumeNow = m.filter { $0.key != "t" }
            lock.unlock()
            onShowing?()
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
        let was = shown
        shown = nil
        docPages = (0, 0)
        lock.unlock()
        onMessage?(["t": "stop", "reason": reason])
        if was != nil { onShowing?() }
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
        let kind = t == "open_app" ? "app" : t
        lock.lock()
        shown = kind
        lock.unlock()
        onShowing?()
        return kind
    }

    /// Pointer, ink, blank and page turns (PROTOCOL.md 4h): on this channel, as only the sender of what is shown may
    /// draw on it.
    public func send(_ m: [String: Any]) {
        ctl.send(m)
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

    /// "0".."100", "+", "-", "mute", "unmute"; the receiver's volume after it. "" only reads it.
    public func volume(_ what: String) throws -> [String: Any] {
        var m: [String: Any] = ["t": "volume"]
        switch what {
        case "":
            guard let v = currentVolume else { throw CastError.bad("the TV keeps its volume to itself") }
            return v
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
