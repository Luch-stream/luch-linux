// The built-in web receiver (luchd's serve_web): `https://<this Mac>:47801/` serves one page; a browser that opens
// it (a TV's will do) and is allowed here becomes a receiver. Its websocket carries the receiver handshake and the
// stream: H.264 and Opus frames with the Luch header, decoded by WebCodecs. To the Engine a browser is a Luch
// receiver on loopback (WebBridge), so mirroring to it is the same session as to a TV.
// SPDX-License-Identifier: GPL-3.0-or-later
import Darwin
import Foundation
import LuchNet
import LuchProto
import LuchTV
import Network

public final class WebReceiver {
    public static let defaultPort: UInt16 = 47801
    /// The local API's commands for it (luchctl web).
    public static let commands: Set<String> = ["web_clients", "approve_web", "forget_web", "web_url"]

    /// A browser waiting to be let in: its page shows `code`, and so does the question here.
    public final class Question {
        public let id = UUID().uuidString
        public let name: String
        public let address: String
        public let code: String
        private let done = DispatchSemaphore(value: 0)
        private let lock = NSLock()
        private var answer: Bool?

        init(name: String, address: String, code: String) {
            self.name = name
            self.address = address
            self.code = code
        }

        func give(_ allow: Bool) {
            lock.lock()
            let first = answer == nil
            if first { answer = allow }
            lock.unlock()
            if first { done.signal() }
        }

        func wait(_ seconds: Double) -> Bool? {
            guard done.wait(timeout: .now() + seconds) == .success else { return nil }
            lock.lock()
            defer { lock.unlock() }
            return answer
        }
    }

    let dir: URL
    private let lock = NSLock()
    private var listener: NWListener?
    private var names: [String] = []
    private var limits = WebAuth.Limits()
    private var waiting: [String: Question] = [:]
    private var bridges: [String: WebBridge] = [:]
    private let queue = DispatchQueue(label: "luch.web")
    public private(set) var port: UInt16 = 0

    /// Ask this Mac's person (the app shows the code with Allow and Deny); answer with `answer`.
    public var onAsk: ((Question) -> Void)?
    /// The question was answered elsewhere (luchctl web allow) or nobody answered in time: take it away.
    public var onAskDone: ((Question) -> Void)?
    /// A browser became a receiver; `mode` is what its page asked for ("fast", "virtual", "none"...).
    public var onReceiver: ((Receiver, _ mode: String) -> Void)?
    /// A browser is no longer a receiver.
    public var onGone: ((Receiver) -> Void)?

    /// `dir` keeps the allowed browsers and the HTTPS identity.
    public init(dir: URL) {
        self.dir = dir
    }

    /// Listens on `port` on every interface.
    public func start(port: UInt16 = WebReceiver.defaultPort) throws {
        let id = try WebIdentity.load(dir, addresses: SSDP.lanAddresses())
        let params = try id.identity.serverParameters()
        params.allowLocalEndpointReuse = true
        let l = try NWListener(using: params, on: NWEndpoint.Port(rawValue: port)!)
        let ready = DispatchSemaphore(value: 0)
        var failure: String?
        l.stateUpdateHandler = { st in
            switch st {
            case .ready: ready.signal()
            case .failed(let e):
                failure = e.localizedDescription
                ready.signal()
            default: break
            }
        }
        l.newConnectionHandler = { [weak self] c in
            Thread.detachNewThread { self?.connection(ByteStream(c)) }
        }
        l.start(queue: queue)
        _ = ready.wait(timeout: .now() + 5)
        if let failure {
            l.cancel()
            throw TVError("cannot listen on \(port): \(failure)")
        }
        lock.lock()
        listener?.cancel()
        listener = l
        names = id.names
        self.port = l.port?.rawValue ?? port
        lock.unlock()
        log("web: receiver at https://<this Mac>:\(self.port)/ (local self-signed certificate)")
    }

    public func stop() {
        lock.lock()
        listener?.cancel()
        listener = nil
        let all = Array(bridges.values)
        lock.unlock()
        all.forEach { $0.end() }
    }

    /// The page's address on this Mac's LAN, nil without one.
    public var url: String? {
        guard let ip = SSDP.lanAddresses().first else { return nil }
        return "https://\(ip):\(port)/"
    }

    /// The browsers that are receivers now.
    public var receivers: [Receiver] {
        lock.lock()
        defer { lock.unlock() }
        return bridges.values.map(\.receiver).sorted { $0.name < $1.name }
    }

    /// The person's answer to question `id`; false when nothing waits under it.
    @discardableResult
    public func answer(_ id: String, allow: Bool) -> Bool {
        lock.lock()
        let q = waiting[id]
        lock.unlock()
        q?.give(allow)
        return q != nil
    }

    /// Whether question `id` still waits for an answer.
    public func isWaiting(_ id: String) -> Bool {
        lock.lock()
        defer { lock.unlock() }
        return waiting[id] != nil
    }

    // MARK: - the local API

    public func handle(_ request: [String: Any]) -> [String: Any] {
        switch request["cmd"] as? String ?? "" {
        case "web_clients":
            lock.lock()
            var rows: [[String: Any]] = waiting.values.map {
                ["id": $0.id, "name": $0.name, "address": $0.address, "code": $0.code, "waiting": true]
            }
            lock.unlock()
            rows += WebAuth.list(dir)
            return ["ok": true, "clients": rows]
        case "approve_web":
            guard let id = request["request"] as? String, let allow = request["allow"] as? Bool else {
                return ["error": "approve_web needs request and allow"]
            }
            return answer(id, allow: allow) ? ["ok": true] : ["error": "no browser waits under \(id)"]
        case "forget_web":
            guard let id = request["id"] as? String else { return ["error": "forget_web needs id"] }
            do {
                return try WebAuth.forget(dir, id: id) ? ["ok": true] : ["error": "no allowed browser \(id)"]
            } catch {
                return ["error": error.localizedDescription]
            }
        case "web_url":
            return url.map { ["ok": true, "url": $0] } ?? ["error": "no network"]
        default:
            return ["error": "unknown command"]
        }
    }

    // MARK: - one browser connection

    /// Any GET serves the page, `POST /auth` sets the cookie, `/ws` upgrades and becomes a receiver.
    private func connection(_ s: ByteStream) {
        defer { s.close() }
        guard (try? s.start(timeout: 10)) != nil, let raw = s.readHead(timeout: 10) else { return }
        // A laptop may join another network: the next browser there gets a certificate with its address.
        refreshIdentity()
        let head = String(decoding: raw, as: UTF8.self)
        let first = head.components(separatedBy: "\r\n").first ?? ""
        let path = first.split(separator: " ").dropFirst().first.map(String.init) ?? "/"
        var headers: [String: String] = [:]
        for l in head.components(separatedBy: "\r\n").dropFirst() {
            guard let colon = l.firstIndex(of: ":") else { continue }
            headers[l[..<colon].trimmingCharacters(in: .whitespaces).lowercased()] = l[l.index(after: colon)...].trimmingCharacters(in: .whitespaces)
        }
        let peer = Self.plain(s.remoteAddress)
        // A browser allowed before carries its cookie; any other is asked about on this Mac first.
        let allowed = WebAuth.cookie(head).map { WebAuth.known(dir, token: $0) } ?? false
        if first.hasPrefix("POST /auth ") {
            let n = min(256, Int(headers["content-length"] ?? "0") ?? 0)
            let token = n > 0 ? String(decoding: s.read(n, timeout: 10) ?? Data(), as: UTF8.self).trimmingCharacters(in: .whitespacesAndNewlines) : ""
            // The token the page was given over its websocket, as an HttpOnly cookie its scripts cannot read.
            let answer = WebAuth.known(dir, token: token)
                ? "HTTP/1.1 204 No Content\r\nSet-Cookie: \(WebAuth.cookieName)=\(token); Max-Age=31536000; Path=/; Secure; HttpOnly; SameSite=Strict\r\nConnection: close\r\n\r\n"
                : "HTTP/1.1 403 Forbidden\r\nContent-Length: 0\r\nConnection: close\r\n\r\n"
            s.write(Data(answer.utf8))
            return
        }
        if headers["upgrade"]?.lowercased() == "websocket", path.split(separator: "?").first == "/ws",
           let key = headers["sec-websocket-key"] {
            guard s.write(WebSocket.handshake(key)) else { return }
            browser(WebSocket(s), peer: peer, allowed: allowed)
            return
        }
        // Any GET (not just /) serves the page: TV browsers append or mangle paths.
        let (code, type, body) = first.hasPrefix("GET ") ? ("200 OK", "text/html; charset=utf-8", Data(WebPage.html.utf8))
            : ("404 Not Found", "text/plain", Data("not found".utf8))
        s.write(Data("HTTP/1.1 \(code)\r\nContent-Type: \(type)\r\nContent-Length: \(body.count)\r\nCache-Control: no-store\r\nConnection: close\r\n\r\n".utf8) + body)
    }

    /// An IPv4 address as a browser reached us over IPv6 sockets ("::ffff:a.b.c.d") made plain.
    static func plain(_ a: String) -> String {
        a.hasPrefix("::ffff:") ? String(a.dropFirst(7)) : a
    }

    private func refreshIdentity() {
        lock.lock()
        let known = names
        let p = port
        lock.unlock()
        guard !SSDP.lanAddresses().allSatisfy(known.contains) else { return }
        // The next connection gets it; this one keeps the certificate it was given.
        DispatchQueue.global().async { [weak self] in try? self?.start(port: p) }
    }

    /// A websocket: its hello, the question here when it is new, then a receiver until it goes.
    private func browser(_ ws: WebSocket, peer: String, allowed: Bool) {
        guard case .text(let t)? = ws.receive(timeout: 5),
              let hello = try? JSONSerialization.jsonObject(with: Data(t.utf8)) as? [String: Any],
              hello["t"] as? String == "hello" else {
            log("web \(peer): no hello")
            ws.close()
            return
        }
        if !allowed {
            if let why = approve(ws, name: hello["model"] as? String ?? "a browser", address: peer) {
                log("web \(peer): not let in (\(why))")
                ws.send(json: ["t": "denied", "reason": why])
                ws.close()
                return
            }
        }
        guard let bridge = try? WebBridge(ws: ws, hello: hello, peer: peer) else {
            ws.send(json: ["t": "error", "msg": "cannot open the way to the stream"])
            ws.close()
            return
        }
        lock.lock()
        bridges[bridge.receiver.id] = bridge
        lock.unlock()
        log("web: \(bridge.receiver.name) (\(peer)) is a receiver")
        onReceiver?(bridge.receiver, hello["mode"] as? String ?? "fast")
        bridge.run()
        lock.lock()
        bridges[bridge.receiver.id] = nil
        lock.unlock()
        log("web: \(bridge.receiver.name) (\(peer)) gone")
        onGone?(bridge.receiver)
    }

    /// Before a browser that is not allowed yet becomes a receiver: the code on its page and in a question here. Nil
    /// with the browser let in (and given its token), else what its page says.
    private func approve(_ ws: WebSocket, name: String, address: String) -> String? {
        let now = WebAuth.unixNow()
        lock.lock()
        let verdict = limits.check(address, now: now)
        lock.unlock()
        switch verdict {
        case .banned(let s): return "refused on the computer; try again in \((s + 59) / 60) min"
        case .busy: return "another browser is waiting to be allowed; try again in a minute"
        case .ask: break
        }
        let q = Question(name: name, address: address, code: WebAuth.code())
        lock.lock()
        waiting[q.id] = q
        lock.unlock()
        ws.send(json: ["t": "confirm", "code": q.code])
        log("web: asking about \(name) (\(address)), code \(q.code)")
        onAsk?(q)
        let answer = q.wait(WebAuth.ask)
        lock.lock()
        waiting[q.id] = nil
        limits.done()
        if answer == false { limits.strike(address, now: now) }
        lock.unlock()
        onAskDone?(q)
        switch answer {
        case true?:
            guard let token = try? WebAuth.allow(dir, name: name, address: address) else { return "cannot remember the browser" }
            ws.send(json: ["t": "authorized", "token": token])
            log("web: \(name) (\(address)) allowed")
            return nil
        case false?: return "refused on the computer"
        case nil: return "nobody answered on the computer"
        }
    }
}
