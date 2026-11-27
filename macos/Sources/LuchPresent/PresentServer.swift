// The presenter view: a page on this Mac for a document shown on a receiver (PROTOCOL.md 4g, 4h), luchd's present.rs
// with the same page (present.html, present.js). It shows the current and the next slide, the speaker notes, a timer
// and the slide counter; keys, clicks and a clicker turn the slides, the mouse over the slide moves the receiver's
// laser pointer and dragging draws on it.
//
// Served over plain HTTP on 127.0.0.1 only, under a random token, and only to a Host of this machine (a web page
// elsewhere cannot reach it through DNS rebinding). The page talks to the app over a websocket: page turns, pointer
// and ink go to the receiver at once, pointer moves coalesced; the receiver's own page turns and blanking come back.
// Slides are drawn with PDFKit, so nothing needs installing on a Mac.
// SPDX-License-Identifier: GPL-3.0-or-later
import AppKit
import Darwin
import Foundation
import LuchProto
import PDFKit

/// A document shown on a receiver for this presenter view.
public final class Show {
    public let receiver: String
    let receiverName: String
    let name: String
    let pdf: URL
    let notes: [String]
    let ink: Bool
    /// A message for the receiver, on the channel that showed the document (only its sender may draw on it).
    let send: ([String: Any]) -> Void

    private let lock = NSLock()
    private var page: Int
    private var pages: Int
    private var blank = false
    private var over = false
    private var views: [ObjectIdentifier: WebSocket] = [:]
    /// Held while it is shown: do not disturb (Quiet), let go when it ends.
    private var hold: AnyObject?
    /// Called once when it is over: the receiver closed it, the sender stopped it, or another show took its place.
    var onEnded: (() -> Void)?

    private let render = NSLock()
    private var document: PDFDocument?
    let cache: URL

    public init(receiver: String, receiverName: String, name: String, pdf: URL, notes: [String], ink: Bool,
                page: Int, pages: Int, hold: AnyObject? = nil, send: @escaping ([String: Any]) -> Void) {
        self.receiver = receiver
        self.receiverName = receiverName
        self.name = name
        self.pdf = pdf
        self.notes = notes
        self.ink = ink
        self.page = page
        self.pages = pages
        self.hold = hold
        self.send = send
        cache = FileManager.default.temporaryDirectory
            .appendingPathComponent("luch-present-\(getpid())-\(UUID().uuidString)", isDirectory: true)
    }

    deinit { try? FileManager.default.removeItem(at: cache) }

    public var isOver: Bool {
        lock.lock()
        defer { lock.unlock() }
        return over
    }

    func hello() -> [String: Any] {
        lock.lock()
        defer { lock.unlock() }
        return ["t": "hello", "name": name, "receiver": receiverName, "page": page, "pages": pages, "blank": blank,
                "notes": notes, "ink": ink]
    }

    var pageCount: Int {
        lock.lock()
        defer { lock.unlock() }
        return pages
    }

    /// Follows what the receiver says; the presenter pages follow it too.
    public func heard(_ m: [String: Any]) {
        let num = { (k: String) in (m[k] as? NSNumber)?.intValue }
        lock.lock()
        switch m["t"] as? String ?? "" {
        case "doc_page":
            guard let p = num("page") else { lock.unlock(); return }
            page = p
            blank = false
        case "doc_ready":
            guard let n = num("pages") else { lock.unlock(); return }
            pages = n
        case "blank":
            guard let on = m["on"] as? Bool else { lock.unlock(); return }
            blank = on
        case "stop":
            lock.unlock()
            end()
            return
        default:
            lock.unlock()
            return
        }
        let state: [String: Any] = ["t": "state", "page": page, "pages": pages, "blank": blank]
        let all = Array(views.values)
        lock.unlock()
        let line = Show.json(state)
        all.forEach { $0.send(line) }
    }

    /// It is over: every presenter page is told and closed, do-not-disturb is let go.
    public func end() {
        lock.lock()
        if over { lock.unlock(); return }
        over = true
        let all = Array(views.values)
        views.removeAll()
        hold = nil
        let done = onEnded
        onEnded = nil
        lock.unlock()
        for ws in all {
            ws.send(Show.json(["t": "ended"]))
            ws.close()
        }
        done?()
    }

    /// Adds a page's websocket; false when the show is already over.
    func attach(_ ws: WebSocket) -> Bool {
        lock.lock()
        defer { lock.unlock() }
        if over { return false }
        views[ObjectIdentifier(ws)] = ws
        return true
    }

    func detach(_ ws: WebSocket) {
        lock.lock()
        views[ObjectIdentifier(ws)] = nil
        lock.unlock()
    }

    static func json(_ v: [String: Any]) -> String {
        (try? JSONSerialization.data(withJSONObject: v)).map { String(decoding: $0, as: UTF8.self) } ?? "{}"
    }

    // MARK: - slides as pictures

    /// Page `page` (from 0) as PNG, `width` pixels wide; cached on disk as luchd does.
    func slide(_ page: Int, width: Int) -> Data? {
        let out = cache.appendingPathComponent("p\(page)-w\(width).png")
        if let d = try? Data(contentsOf: out) { return d }
        render.lock()
        defer { render.unlock() }
        if document == nil { document = PDFDocument(url: pdf) }
        guard let png = document.flatMap({ Show.png($0, page: page, width: width) }) else { return nil }
        try? FileManager.default.createDirectory(at: cache, withIntermediateDirectories: true)
        try? png.write(to: out)
        return png
    }

    static func png(_ doc: PDFDocument, page: Int, width: Int) -> Data? {
        guard page >= 0, page < doc.pageCount, let p = doc.page(at: page) else { return nil }
        let box = p.bounds(for: .cropBox)
        let turned = p.rotation % 180 != 0
        let (w, h) = turned ? (box.height, box.width) : (box.width, box.height)
        guard w > 0, h > 0 else { return nil }
        let scale = CGFloat(width) / w
        guard let ctx = CGContext(data: nil, width: width, height: max(1, Int((h * scale).rounded())), bitsPerComponent: 8,
                                  bytesPerRow: 0, space: CGColorSpace(name: CGColorSpace.sRGB)!,
                                  bitmapInfo: CGImageAlphaInfo.premultipliedFirst.rawValue
                                      | CGBitmapInfo.byteOrder32Little.rawValue) else { return nil }
        ctx.setFillColor(.white)
        ctx.fill(CGRect(x: 0, y: 0, width: ctx.width, height: ctx.height))
        ctx.scaleBy(x: scale, y: scale)
        // The crop box's origin and the page's rotation, as a viewer shows it.
        p.transform(ctx, for: .cropBox)
        p.draw(with: .cropBox, to: ctx)
        guard let cg = ctx.makeImage() else { return nil }
        return NSBitmapImageRep(cgImage: cg).representation(using: .png, properties: [:])
    }

    /// Widths in steps, so a resized window does not render every slide again for each pixel.
    static func widthStep(_ asked: Int) -> Int {
        (min(max(asked, 160), 3840) + 159) / 160 * 160
    }
}

public final class PresentServer {
    public private(set) var port: UInt16 = 0
    private var listenFD: Int32 = -1
    private let lock = NSLock()
    private var shows: [String: Show] = [:]

    public init() {}

    /// Listens on 127.0.0.1, on any free port.
    public func start() throws {
        let fd = socket(AF_INET, SOCK_STREAM, 0)
        guard fd >= 0 else { throw PresentError.bad("presenter view: \(errnoString())") }
        var yes: Int32 = 1
        setsockopt(fd, SOL_SOCKET, SO_REUSEADDR, &yes, socklen_t(MemoryLayout<Int32>.size))
        var a = sockaddr_in()
        a.sin_len = UInt8(MemoryLayout<sockaddr_in>.size)
        a.sin_family = sa_family_t(AF_INET)
        a.sin_port = 0
        a.sin_addr.s_addr = inet_addr("127.0.0.1")
        let bound = withUnsafePointer(to: &a) {
            $0.withMemoryRebound(to: sockaddr.self, capacity: 1) { bind(fd, $0, socklen_t(MemoryLayout<sockaddr_in>.size)) }
        }
        guard bound == 0, listen(fd, 16) == 0 else {
            close(fd)
            throw PresentError.bad("presenter view: \(errnoString())")
        }
        var len = socklen_t(MemoryLayout<sockaddr_in>.size)
        withUnsafeMutablePointer(to: &a) { $0.withMemoryRebound(to: sockaddr.self, capacity: 1) { _ = getsockname(fd, $0, &len) } }
        port = UInt16(bigEndian: a.sin_port)
        listenFD = fd
        Thread.detachNewThread { [weak self] in self?.acceptLoop(fd) }
    }

    public func stop() {
        if listenFD >= 0 { close(listenFD) }
        listenFD = -1
    }

    /// Adds a show (ending the one on the same receiver) and returns its page's address.
    public func add(_ show: Show) -> String {
        let token = UUID().uuidString.replacingOccurrences(of: "-", with: "").lowercased()
        end(receiver: show.receiver)
        show.onEnded = { [weak self] in
            guard let self else { return }
            self.lock.lock()
            self.shows[token] = nil
            self.lock.unlock()
        }
        lock.lock()
        shows[token] = show
        lock.unlock()
        return "http://127.0.0.1:\(port)/\(token)/"
    }

    /// The show on a receiver is over (the sender stopped it, or shows another).
    public func end(receiver: String) {
        lock.lock()
        let old = shows.values.filter { $0.receiver == receiver }
        lock.unlock()
        old.forEach { $0.end() }
    }

    /// The show on a receiver, if one runs.
    public func show(on receiver: String) -> (show: Show, url: String)? {
        lock.lock()
        defer { lock.unlock() }
        return shows.first { $0.value.receiver == receiver }.map { ($0.value, "http://127.0.0.1:\(port)/\($0.key)/") }
    }

    /// Only this machine's names: a page elsewhere that points a host name at 127.0.0.1 gets nothing.
    static func localHost(_ host: String, port: UInt16) -> Bool {
        ["127.0.0.1", "localhost", "[::1]"].contains { "\($0):\(port)".caseInsensitiveCompare(host) == .orderedSame }
    }

    private func acceptLoop(_ listening: Int32) {
        while listenFD == listening {
            let fd = accept(listening, nil, nil)
            if fd < 0 { if listenFD != listening { return }; continue }
            var yes: Int32 = 1
            setsockopt(fd, SOL_SOCKET, SO_NOSIGPIPE, &yes, socklen_t(MemoryLayout<Int32>.size))
            Thread.detachNewThread { [weak self] in
                self?.connection(fd)
                close(fd)
            }
        }
    }

    private func connection(_ fd: Int32) {
        var tv = timeval(tv_sec: 5, tv_usec: 0)
        setsockopt(fd, SOL_SOCKET, SO_RCVTIMEO, &tv, socklen_t(MemoryLayout<timeval>.size))
        var carry = Data()
        guard let q = HTTP.read(fd, carry: &carry) else { return }
        guard q.method == "GET", q.headers["host"].map({ PresentServer.localHost($0, port: port) }) == true else {
            return HTTP.reply(fd, "403 Forbidden", "text/plain", Data("forbidden".utf8))
        }
        let (path, query) = q.target.firstIndex(of: "?").map { (String(q.target[..<$0]), String(q.target[q.target.index(after: $0)...])) }
            ?? (q.target, "")
        let parts = path.drop(while: { $0 == "/" }).split(separator: "/", maxSplits: 1, omittingEmptySubsequences: false)
        let token = parts.first.map(String.init) ?? ""
        let rest = parts.count > 1 ? String(parts[1]) : ""
        lock.lock()
        let show = shows[token]
        lock.unlock()
        guard let show else {
            return HTTP.reply(fd, "404 Not Found", "text/plain", Data("this presentation is over".utf8))
        }
        switch rest {
        case "":
            HTTP.reply(fd, "200 OK", "text/html; charset=utf-8", Data(PresentPage.html.utf8))
        case "present.js":
            HTTP.reply(fd, "200 OK", "text/javascript; charset=utf-8", Data(PresentPage.script.utf8))
        case "ws" where q.headers["upgrade"]?.lowercased() == "websocket":
            // A websocket is not bound by the same-origin rule: only our own page may open one.
            let origin = q.headers["origin"].flatMap { $0.hasPrefix("http://") ? String($0.dropFirst(7)) : nil }
            guard let origin, PresentServer.localHost(origin, port: port), let key = q.headers["sec-websocket-key"] else {
                return HTTP.reply(fd, "403 Forbidden", "text/plain", Data("forbidden".utf8))
            }
            guard HTTP.write(fd, WebSocket.handshake(key: key)) else { return }
            var none = timeval(tv_sec: 0, tv_usec: 0)
            setsockopt(fd, SOL_SOCKET, SO_RCVTIMEO, &none, socklen_t(MemoryLayout<timeval>.size))
            session(WebSocket(fd: fd, carry: carry), show)
        case let s where s.hasPrefix("slide/"):
            let page = Int(s.dropFirst(6).replacingOccurrences(of: ".png", with: ""))
            let width = query.split(separator: "&").lazy.compactMap { $0.hasPrefix("w=") ? Int($0.dropFirst(2)) : nil }
                .first ?? 1280
            guard let page, page >= 0, page < max(show.pageCount, 1) else {
                return HTTP.reply(fd, "404 Not Found", "text/plain", Data("no such slide".utf8))
            }
            if let png = show.slide(page, width: Show.widthStep(width)) {
                HTTP.reply(fd, "200 OK", "image/png", png)
            } else {
                HTTP.reply(fd, "503 Service Unavailable", "text/plain", Data("cannot draw this page".utf8))
            }
        default:
            HTTP.reply(fd, "404 Not Found", "text/plain", Data("not found".utf8))
        }
    }

    /// One presenter page: its input to the receiver; the receiver's page turns come back through `Show.heard`.
    private func session(_ ws: WebSocket, _ show: Show) {
        guard ws.send(Show.json(show.hello())), show.attach(ws) else {
            ws.send(Show.json(["t": "ended"]))
            return ws.close()
        }
        defer { show.detach(ws) }
        let pointer = PointerGate(send: show.send)
        while let text = ws.receive() {
            guard let v = (try? JSONSerialization.jsonObject(with: Data(text.utf8))) as? [String: Any],
                  let m = Annotate.sanitize(v), !show.isOver else { continue }
            if m["t"] as? String == "pointer" { pointer.offer(m) } else { show.send(m) }
        }
    }
}

/// A Coalescer on a queue of its own: the newest pointer move goes out when its turn comes.
final class PointerGate {
    private var c = Coalescer()
    private let queue = DispatchQueue(label: "luch.present.pointer")
    private var waiting = false
    private let send: ([String: Any]) -> Void

    init(send: @escaping ([String: Any]) -> Void) {
        self.send = send
    }

    private static func now() -> TimeInterval { ProcessInfo.processInfo.systemUptime }

    func offer(_ m: [String: Any]) {
        queue.async { [self] in
            if let out = c.offer(PointerGate.now(), m) { send(out) } else { schedule() }
        }
    }

    private func schedule() {
        let now = PointerGate.now()
        guard !waiting, let d = c.deadline(now: now) else { return }
        waiting = true
        queue.asyncAfter(deadline: .now() + max(0, d - now)) { [self] in
            waiting = false
            if let out = c.due(PointerGate.now()) { send(out) } else { schedule() }
        }
    }
}

public enum PresentError: Error, LocalizedError {
    case bad(String)
    public var errorDescription: String? {
        switch self { case .bad(let s): return s }
    }
}
