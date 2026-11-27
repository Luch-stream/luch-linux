// The web receiver: who may watch (luchd's web_auth.rs tests), the page (the same file as luchd's), the HTTPS
// identity, and a browser's whole way: the code, the question answered here, the cookie, then the stream through the
// bridge to a stand-in for the Engine, which talks to it as to any Luch receiver.
// SPDX-License-Identifier: GPL-3.0-or-later
import Foundation
import Network
import XCTest
@testable import LuchNet
@testable import LuchProto
@testable import LuchTV
@testable import LuchWeb

/// A browser's side of a websocket: frames out masked, as RFC 6455 wants of a client.
final class TestBrowser {
    let s: ByteStream

    init(port: UInt16, path: String = "/ws?mode=none", cookie: String? = nil) throws {
        s = try ByteStream.connect(host: "127.0.0.1", port: port, parameters: ByteStream.tlsAnyCertificate(), timeout: 5)
        var req = "GET \(path) HTTP/1.1\r\nHost: 127.0.0.1\r\nUpgrade: websocket\r\nConnection: Upgrade\r\n"
            + "Sec-WebSocket-Version: 13\r\nSec-WebSocket-Key: dGhlIHNhbXBsZSBub25jZQ==\r\n"
        if let cookie { req += "Cookie: a=1; \(cookie)\r\n" }
        s.write(Data((req + "\r\n").utf8))
        let head = String(decoding: s.readHead(timeout: 5) ?? Data(), as: UTF8.self)
        guard head.hasPrefix("HTTP/1.1 101"), head.contains("s3pPLMBiTxaQ9kYGzzhZRbK+xOo=") else { throw TVError("no upgrade: \(head)") }
    }

    func send(_ v: [String: Any]) {
        let payload = try! JSONSerialization.data(withJSONObject: v)
        let mask: [UInt8] = [1, 2, 3, 4]
        var f = Data([0x81])
        if payload.count < 126 { f.append(0x80 | UInt8(payload.count)) } else {
            f.append(0x80 | 126)
            f.append(contentsOf: [UInt8(payload.count >> 8), UInt8(payload.count & 0xff)])
        }
        f.append(contentsOf: mask)
        f.append(contentsOf: payload.enumerated().map { $0.element ^ mask[$0.offset & 3] })
        s.write(f)
    }

    /// The next message: JSON for text, raw bytes for binary; nil at the end.
    func next(timeout: Double = 10) -> Any? {
        guard let h = s.read(2, timeout: timeout) else { return nil }
        var n = Int(h[1] & 0x7f)
        if n == 126 { let e = s.read(2)!; n = Int(e[0]) << 8 | Int(e[1]) }
        if n == 127 { n = Int(s.read(8)!.reduce(UInt64(0)) { $0 << 8 | UInt64($1) }) }
        let body = n > 0 ? s.read(n, timeout: timeout) ?? Data() : Data()
        switch h[0] & 0x0f {
        case 1: return try? JSONSerialization.jsonObject(with: body) as? [String: Any]
        case 2: return body
        case 8: return nil
        default: return next(timeout: timeout)
        }
    }

    /// The next text message of type `t`, skipping others (pings answered).
    func expect(_ t: String, timeout: Double = 10) -> [String: Any]? {
        let until = Date().addingTimeInterval(timeout)
        while Date() < until {
            guard let m = next(timeout: until.timeIntervalSinceNow) else { return nil }
            guard let v = m as? [String: Any] else { continue }
            if v["t"] as? String == t { return v }
            if v["t"] as? String == "ping" { send(["t": "pong", "n": v["n"] ?? 0]) }
        }
        return nil
    }
}

final class WebTests: XCTestCase {
    private func dir() -> URL {
        let d = FileManager.default.temporaryDirectory.appendingPathComponent("luch-web-\(UUID().uuidString)")
        try? FileManager.default.createDirectory(at: d, withIntermediateDirectories: true)
        return d
    }

    func testAnAllowedBrowserIsKnownByItsTokenOnly() throws {
        let d = dir()
        let token = try WebAuth.allow(d, name: "Chrome/120", address: "192.168.1.30")
        XCTAssertTrue(WebAuth.known(d, token: token))
        XCTAssertFalse(WebAuth.known(d, token: "0000"))
        let text = try String(contentsOf: d.appendingPathComponent("web-browsers.json"), encoding: .utf8)
        XCTAssertFalse(text.contains(token), "only a hash of the token is kept")
        let listed = WebAuth.list(d)
        XCTAssertEqual(listed.count, 1)
        XCTAssertNil(listed[0]["hash"])
        XCTAssertTrue(try WebAuth.forget(d, id: listed[0]["id"] as! String))
        XCTAssertFalse(WebAuth.known(d, token: token))
    }

    func testTwentyBrowsersAtMost() throws {
        let d = dir()
        let first = try WebAuth.allow(d, name: "first", address: "192.168.1.2")
        let second = try WebAuth.allow(d, name: "second", address: "192.168.1.2")
        for i in 0..<WebAuth.max - 1 { _ = try WebAuth.allow(d, name: "b\(i)", address: "192.168.1.3") }
        XCTAssertEqual(WebAuth.load(d).count, WebAuth.max)
        XCTAssertFalse(WebAuth.known(d, token: first), "the least recently used is forgotten")
        XCTAssertTrue(WebAuth.known(d, token: second))
    }

    func testTheCookieIsFoundAmongOthers() {
        XCTAssertEqual(WebAuth.cookie("GET /ws HTTP/1.1\r\nHost: x\r\nCookie: a=1; luch_web=abc123; b=2\r\n\r\n"), "abc123")
        XCTAssertEqual(WebAuth.cookie("GET / HTTP/1.1\r\ncookie: luch_web=z\r\n\r\n"), "z")
        XCTAssertNil(WebAuth.cookie("GET / HTTP/1.1\r\nCookie: other=1\r\n\r\n"))
    }

    func testThreeRefusalsAndAnAddressWaitsAnHour() {
        var l = WebAuth.Limits()
        let t: UInt64 = 1_000_000
        for _ in 0..<WebAuth.strikes {
            XCTAssertEqual(l.check("192.168.1.30", now: t), .ask)
            l.strike("192.168.1.30", now: t)
            l.done()
        }
        XCTAssertEqual(l.check("192.168.1.30", now: t + 10), .banned(WebAuth.ban - 10))
        XCTAssertEqual(l.check("192.168.1.31", now: t), .ask)
        XCTAssertEqual(l.check("192.168.1.32", now: t), .busy, "one question at a time")
        l.done()
        XCTAssertEqual(l.check("192.168.1.30", now: t + WebAuth.ban), .ask)
    }

    func testCodesAreFourDigits() {
        for _ in 0..<50 {
            let c = WebAuth.code()
            XCTAssertTrue(c.count == 4 && c.allSatisfy(\.isNumber))
        }
    }

    /// Both senders serve the one page.
    func testThePageIsLuchdsPage() throws {
        let luchd = URL(fileURLWithPath: #filePath).deletingLastPathComponent().appendingPathComponent("../../../luchd/src/web.html")
        XCTAssertEqual(WebPage.html, try String(contentsOf: luchd, encoding: .utf8))
    }

    func testTheWebSocketAcceptKeyIsRFC6455s() {
        XCTAssertEqual(WebSocket.accept("dGhlIHNhbXBsZSBub25jZQ=="), "s3pPLMBiTxaQ9kYGzzhZRbK+xOo=")
        XCTAssertEqual(WebSocket.frame(1, Data("Hi".utf8)), Data([0x81, 2, 0x48, 0x69]))
        XCTAssertEqual(WebSocket.frame(2, Data(count: 300)).prefix(4), Data([0x82, 126, 1, 44]))
    }

    /// The certificate made by hand is one openssl reads, for the names given, and it is kept across restarts.
    func testTheHTTPSIdentityIsAProperCertificateAndStays() throws {
        let d = dir()
        let first = try WebIdentity.load(d, addresses: ["192.168.1.20"])
        let der = d.appendingPathComponent("web-cert.der")
        let p = Process()
        p.executableURL = URL(fileURLWithPath: "/usr/bin/openssl")
        p.arguments = ["x509", "-inform", "DER", "-in", der.path, "-noout", "-text"]
        let out = Pipe()
        p.standardOutput = out
        p.standardError = out
        try p.run()
        p.waitUntilExit()
        let text = String(decoding: out.fileHandleForReading.readDataToEndOfFile(), as: UTF8.self)
        XCTAssertEqual(p.terminationStatus, 0, text)
        XCTAssertTrue(text.contains("IP Address:192.168.1.20") && text.contains("DNS:localhost"), text)
        XCTAssertTrue(text.contains("ecdsa-with-SHA256") && text.contains("Luch local receiver"), text)
        let attrs = try FileManager.default.attributesOfItem(atPath: d.appendingPathComponent("web-key.x963").path)
        XCTAssertEqual((attrs[.posixPermissions] as? NSNumber)?.intValue, 0o600)
        let again = try WebIdentity.load(d, addresses: ["192.168.1.20"])
        XCTAssertEqual(again.identity.certificate, first.identity.certificate, "the same names: the same certificate")
        let moved = try WebIdentity.load(d, addresses: ["10.0.0.5"])
        XCTAssertNotEqual(moved.identity.certificate, first.identity.certificate)
        XCTAssertEqual(moved.names, ["localhost", "127.0.0.1", "192.168.1.20", "10.0.0.5"])
        XCTAssertEqual(moved.identity.keyData, first.identity.keyData, "one key")
    }

    /// A browser's whole way: the page, the code and the question here, the answer, the cookie; then the bridge,
    /// with a stand-in Engine that connects to it as to a Luch receiver, starts the stream and sends a frame in shards.
    func testABrowserIsAskedAboutThenWatches() throws {
        let web = WebReceiver(dir: dir())
        let asked = DispatchSemaphore(value: 0)
        var question: WebReceiver.Question?
        web.onAsk = { q in
            question = q
            asked.signal()
        }
        let became = DispatchSemaphore(value: 0)
        var receiver: Receiver?
        web.onReceiver = { r, mode in
            receiver = r
            XCTAssertEqual(mode, "none")
            became.signal()
        }
        try web.start(port: 0)
        defer { web.stop() }

        // Any GET is the page.
        let get = try ByteStream.connect(host: "127.0.0.1", port: web.port, parameters: ByteStream.tlsAnyCertificate(), timeout: 5)
        get.write(Data("GET /some/path HTTP/1.1\r\nHost: x\r\n\r\n".utf8))
        let page = String(decoding: get.readHead(timeout: 5) ?? Data(), as: UTF8.self)
        XCTAssertTrue(page.hasPrefix("HTTP/1.1 200") && page.contains("text/html"), page)

        let b = try TestBrowser(port: web.port)
        b.send(["t": "hello", "v": 0, "name": "Web", "model": "Chrome/130", "display": ["width": 1920, "height": 1080], "mode": "none"])
        let confirm = try XCTUnwrap(b.expect("confirm"))
        XCTAssertEqual(asked.wait(timeout: .now() + 5), .success)
        let q = try XCTUnwrap(question)
        XCTAssertEqual(q.code, confirm["code"] as? String, "the same code here and on the page")
        XCTAssertEqual(q.address, "127.0.0.1")
        // What `luchctl web` lists, and `luchctl web allow <code>` answers.
        let clients = web.handle(["cmd": "web_clients"])["clients"] as? [[String: Any]] ?? []
        XCTAssertEqual(clients.first?["code"] as? String, q.code)
        XCTAssertEqual(clients.first?["waiting"] as? Bool, true)
        XCTAssertNil(web.handle(["cmd": "approve_web", "request": q.id, "allow": true])["error"])
        let token = try XCTUnwrap(b.expect("authorized")?["token"] as? String)
        XCTAssertEqual(became.wait(timeout: .now() + 5), .success)
        let r = try XCTUnwrap(receiver)
        XCTAssertTrue(r.id.hasPrefix("web-"))
        XCTAssertEqual(web.receivers.map(\.id), [r.id])

        // The page turns the token into a cookie it cannot read.
        let auth = try ByteStream.connect(host: "127.0.0.1", port: web.port, parameters: ByteStream.tlsAnyCertificate(), timeout: 5)
        auth.write(Data("POST /auth HTTP/1.1\r\nHost: x\r\nContent-Length: \(token.utf8.count)\r\n\r\n\(token)".utf8))
        let set = String(decoding: auth.readHead(timeout: 5) ?? Data(), as: UTF8.self)
        XCTAssertTrue(set.hasPrefix("HTTP/1.1 204") && set.contains("Set-Cookie: luch_web=\(token);") && set.contains("HttpOnly"), set)

        // The Engine's side: a Luch receiver on loopback.
        let ctl = try XCTUnwrap(ControlChannel.open(ip: r.address, port: r.port, interfaceIndex: r.interfaceIndex))
        ctl.send(["t": "hello", "v": 0, "id": "test", "name": "Mac", "app": Proto.app, "want": ["mirror"]])
        let welcome = try XCTUnwrap(ctl.waitFor("welcome", timeout: 5))
        XCTAssertEqual(welcome["decoders"] as? [String], ["video/avc"])
        ctl.send(["t": "start", "mode": "mirror", "video": ["codec": "video/avc", "width": 1920, "height": 1080, "fps": 60]])
        XCTAssertNotNil(b.expect("start"))
        b.send(["t": "ready", "video_port": 0, "audio_port": 0])
        let ready = try XCTUnwrap(ctl.waitFor("ready", timeout: 5))
        let videoPort = try XCTUnwrap((ready["video_port"] as? NSNumber)?.uint16Value)
        XCTAssertGreaterThan(videoPort, 0, "the bridge's own port, not the page's 0")
        let tx = try XCTUnwrap(UdpSender.open(kind: .video, ip: "127.0.0.1", port: videoPort, interfaceIndex: r.interfaceIndex, dscp: 0))
        let frame = (0..<5000).map { UInt8($0 % 251) }
        tx.send(frame: frame, flags: .idr, ptsUS: 123_456, hostLat: 7)
        var got: Data?
        while got == nil, let m = b.next() { got = m as? Data }
        let media = try XCTUnwrap(got, "a whole frame over the websocket")
        XCTAssertEqual(media.count, Proto.headerSize + frame.count)
        XCTAssertEqual(media[0], MediaKind.video.rawValue)
        XCTAssertEqual(media[1], PacketFlags.idr.rawValue)
        XCTAssertEqual(Array(media[12..<16]), [0x00, 0x01, 0xe2, 0x40], "pts as web.html reads it")
        XCTAssertEqual(Array(media.dropFirst(Proto.headerSize)), frame)

        // The page goes: the Engine is told the receiver left.
        b.send(["t": "bye"])
        XCTAssertNotNil(ctl.waitFor("bye", timeout: 5))

        // Next time the cookie lets it straight in.
        let again = try TestBrowser(port: web.port, cookie: "luch_web=\(token)")
        again.send(["t": "hello", "v": 0, "name": "Web", "model": "Chrome/130", "mode": "none"])
        XCTAssertEqual(became.wait(timeout: .now() + 5), .success, "no question for a known browser")
        XCTAssertEqual(web.receivers.count, 1)
    }

    func testARefusedBrowserIsToldSo() throws {
        let web = WebReceiver(dir: dir())
        web.onAsk = { q in web.answer(q.id, allow: false) }
        try web.start(port: 0)
        defer { web.stop() }
        let b = try TestBrowser(port: web.port)
        b.send(["t": "hello", "v": 0, "name": "Web", "model": "Chrome/130"])
        XCTAssertNotNil(b.expect("confirm"))
        XCTAssertEqual(b.expect("denied")?["reason"] as? String, "refused on the computer")
    }
}
