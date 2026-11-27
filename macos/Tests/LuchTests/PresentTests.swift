// The presenter view and do-not-disturb: luchd's present.rs and quiet.rs tests, the same vectors.
// SPDX-License-Identifier: GPL-3.0-or-later
import Darwin
import Foundation
import PDFKit
import XCTest
@testable import LuchPresent

final class PresentTests: XCTestCase {
    private func same(_ a: [String: Any]?, _ b: [String: Any]?, _ why: String = "", line: UInt = #line) {
        XCTAssertEqual(a.map { NSDictionary(dictionary: $0) }, b.map { NSDictionary(dictionary: $0) }, why, line: line)
    }

    func testMessagesAreCleanedBeforeTheReceiverSeesThem() {
        same(Annotate.sanitize(["t": "pointer", "x": 0.123456, "y": 1.7, "evil": 1]), ["t": "pointer", "x": 0.1235, "y": 1.0])
        same(Annotate.sanitize(["t": "pointer", "show": false]), ["t": "pointer", "show": false])
        same(Annotate.sanitize(["t": "pointer", "x": "a", "y": 0]), nil)
        same(Annotate.sanitize(["t": "ink", "do": "stroke", "id": 3, "color": "#E53935", "width": 0.004,
                                "points": [0.1, 0.2, -1, 2]]),
             ["t": "ink", "do": "stroke", "id": 3, "points": [0.1, 0.2, 0.0, 1.0], "color": "#e53935", "width": 0.004])
        // A bad colour or width is dropped, the stroke kept; odd or too many points are refused.
        let s = Annotate.sanitize(["t": "ink", "do": "stroke", "id": 1, "color": "red", "width": 5, "points": [Any]()])
        XCTAssertNotNil(s)
        XCTAssertNil(s?["color"])
        XCTAssertNil(s?["width"])
        same(Annotate.sanitize(["t": "ink", "do": "stroke", "id": 1, "points": [0.1]]), nil)
        same(Annotate.sanitize(["t": "ink", "do": "stroke", "id": 1, "points": Array(repeating: 0.5, count: 4002)]), nil)
        same(Annotate.sanitize(["t": "ink", "do": "undo", "id": 9]), ["t": "ink", "do": "undo"])
        same(Annotate.sanitize(["t": "blank", "on": true]), ["t": "blank", "on": true])
        same(Annotate.sanitize(["t": "blank", "on": 1]), nil, "a boolean, not a number")
        same(Annotate.sanitize(["t": "doc_page", "page": 4]), ["t": "doc_page", "page": 4])
        same(Annotate.sanitize(["t": "doc_page", "page": -1]), nil)
        same(Annotate.sanitize(["t": "stop"]), nil, "the page cannot send anything else")
        XCTAssertNil(Annotate.sanitize(["t": "ink", "do": "stroke", "id": 1, "color": "#12345", "points": [Any]()])?["color"])
    }

    func testPointerMovesAreCoalescedToTheNewest() {
        let ms = { (n: Int) in Double(n) / 1000 }
        let p = { (x: Double) -> [String: Any] in ["t": "pointer", "x": x, "y": 0.5] }
        var c = Coalescer()
        same(c.offer(ms(0), p(0.1)), p(0.1), "the first goes at once")
        same(c.offer(ms(2), p(0.2)), nil)
        same(c.offer(ms(5), p(0.3)), nil)
        XCTAssertEqual(c.deadline(now: ms(5))!, ms(16), accuracy: 1e-9)
        same(c.due(ms(10)), nil, "not yet")
        same(c.due(ms(16)), p(0.3), "only the newest of those")
        XCTAssertNil(c.deadline(now: ms(16)))
        same(c.offer(ms(40), p(0.4)), p(0.4), "after a pause it goes at once again")
        // 1000 moves in a second at 1 ms intervals come out as about 60.
        var sent = 0
        c = Coalescer()
        for n in 0..<1000 {
            if c.offer(ms(n), p(Double(n) / 1000)) != nil { sent += 1 }
            if c.due(ms(n)) != nil { sent += 1 }
        }
        XCTAssertTrue((55...65).contains(sent), "\(sent)")
        same(c.due(ms(2000)), p(0.999), "the last position is never lost")
    }

    func testOnlyThisMachineMayAsk() {
        XCTAssertTrue(PresentServer.localHost("127.0.0.1:4000", port: 4000))
        XCTAssertTrue(PresentServer.localHost("LOCALHOST:4000", port: 4000))
        XCTAssertFalse(PresentServer.localHost("127.0.0.1:4001", port: 4000))
        XCTAssertFalse(PresentServer.localHost("attacker.example:4000", port: 4000))
        XCTAssertEqual(Show.widthStep(1), 160)
        XCTAssertEqual(Show.widthStep(1281), 1440)
        XCTAssertEqual(Show.widthStep(99999), 3840)
    }

    func testSlidesAreDrawnAsWideAsAskedAndTurnedAsShown() {
        let doc = PDFDocument()
        let page = PDFPage()
        page.setBounds(CGRect(x: 0, y: 0, width: 400, height: 300), for: .mediaBox)
        doc.insert(page, at: 0)
        let size = { (png: Data?) -> [Int] in
            guard let png else { return [] }
            return [16, 20].map { at in png.subdata(in: at..<(at + 4)).reduce(0) { $0 << 8 | Int($1) } }
        }
        XCTAssertEqual(size(Show.png(doc, page: 0, width: 320)), [320, 240])
        page.rotation = 90
        XCTAssertEqual(size(Show.png(doc, page: 0, width: 300)), [300, 400], "a turned page is drawn standing")
        XCTAssertNil(Show.png(doc, page: 1, width: 320))
    }

    func testTheWebsocketKeyIsAnsweredAsTheRfcSays() {
        XCTAssertEqual(WebSocket.accept(key: "dGhlIHNhbXBsZSBub25jZQ=="), "s3pPLMBiTxaQ9kYGzzhZRbK+xOo=")
    }

    func testThePageIsLuchdsOwn() throws {
        let src = URL(fileURLWithPath: #filePath).deletingLastPathComponent()
            .appendingPathComponent("../../../luchd/src").standardizedFileURL
        for (file, copy) in [("present.html", PresentPage.html), ("present.js", PresentPage.script)] {
            let original = try String(contentsOf: src.appendingPathComponent(file), encoding: .utf8)
            XCTAssertEqual(copy + "\n", original, "\(file) changed in luchd: run macos/Tools/sync-present.sh")
        }
    }

    // MARK: - the server, end to end

    /// A raw request: (status, body).
    private func get(_ port: UInt16, _ path: String, host: String, extra: String = "") -> (Int, Data) {
        let fd = socket(AF_INET, SOCK_STREAM, 0)
        defer { close(fd) }
        var a = sockaddr_in()
        a.sin_len = UInt8(MemoryLayout<sockaddr_in>.size)
        a.sin_family = sa_family_t(AF_INET)
        a.sin_port = port.bigEndian
        a.sin_addr.s_addr = inet_addr("127.0.0.1")
        let ok = withUnsafePointer(to: &a) {
            $0.withMemoryRebound(to: sockaddr.self, capacity: 1) { connect(fd, $0, socklen_t(MemoryLayout<sockaddr_in>.size)) }
        }
        guard ok == 0 else { return (0, Data()) }
        HTTP.write(fd, Data("GET \(path) HTTP/1.1\r\nHost: \(host)\r\n\(extra)\r\n".utf8))
        var all = Data()
        var buf = [UInt8](repeating: 0, count: 65536)
        while true {
            let n = recv(fd, &buf, buf.count, 0)
            if n <= 0 { break }
            all.append(contentsOf: buf[0..<n])
        }
        let text = String(decoding: all.prefix(64), as: UTF8.self)
        let code = Int(text.split(separator: " ").dropFirst().first ?? "") ?? 0
        let body = all.range(of: Data("\r\n\r\n".utf8)).map { Data(all[$0.upperBound...]) } ?? Data()
        return (code, body)
    }

    /// A two-page PDF made with PDFKit.
    private func pdf() -> URL {
        let doc = PDFDocument()
        doc.insert(PDFPage(), at: 0)
        doc.insert(PDFPage(), at: 1)
        let u = FileManager.default.temporaryDirectory.appendingPathComponent("luch-present-\(UUID().uuidString).pdf")
        doc.write(to: u)
        return u
    }

    final class Sent {
        private let lock = NSLock()
        private var all: [[String: Any]] = []
        func add(_ m: [String: Any]) { lock.lock(); all.append(m); lock.unlock() }
        var list: [[String: Any]] { lock.lock(); defer { lock.unlock() }; return all }
    }

    final class Held {}

    func testThePageDrivesTheReceiverAndFollowsIt() throws {
        let srv = PresentServer()
        try srv.start()
        defer { srv.stop() }
        let sent = Sent()
        weak var held: Held?
        var hold: Held? = Held()
        held = hold
        let show = Show(receiver: "tv", receiverName: "Living room TV", name: "talk.pdf", pdf: pdf(),
                        notes: ["Hello", ""], ink: true, page: 0, pages: 2, hold: hold, send: sent.add)
        hold = nil
        let url = srv.add(show)
        let port = srv.port
        let host = "127.0.0.1:\(port)"
        let path = String(url.dropFirst("http://\(host)".count))
        XCTAssertEqual(get(port, path, host: host).0, 200)
        XCTAssertTrue(String(decoding: get(port, path + "present.js", host: host).1, as: UTF8.self).contains("export"))
        XCTAssertEqual(get(port, path, host: "evil.example:80").0, 403, "DNS rebinding")
        XCTAssertEqual(get(port, "/wrong-token/", host: host).0, 404)
        XCTAssertEqual(get(port, path + "slide/9.png", host: host).0, 404, "no such page")
        let (code, png) = get(port, path + "slide/1.png?w=300", host: host)
        XCTAssertEqual(code, 200)
        XCTAssertEqual(png.prefix(4), Data([0x89, 0x50, 0x4e, 0x47]))
        let width = png.subdata(in: 16..<20).reduce(0) { $0 << 8 | Int($1) }
        XCTAssertEqual(width, 320, "as wide as asked, in steps")
        // A websocket from another site's page is refused.
        let wsHead = "Upgrade: websocket\r\nConnection: Upgrade\r\nSec-WebSocket-Version: 13\r\nSec-WebSocket-Key: dGhlIHNhbXBsZSBub25jZQ==\r\n"
        XCTAssertEqual(get(port, path + "ws", host: host, extra: wsHead + "Origin: http://evil.example\r\n").0, 403)

        var req = URLRequest(url: URL(string: "ws://\(host)\(path)ws")!)
        req.setValue("http://\(host)", forHTTPHeaderField: "Origin")
        let ws = URLSession(configuration: .ephemeral).webSocketTask(with: req)
        ws.resume()
        func next() throws -> [String: Any] {
            let got = DispatchSemaphore(value: 0)
            var out: [String: Any] = [:]
            ws.receive { r in
                if case .success(.string(let s)) = r {
                    out = (try? JSONSerialization.jsonObject(with: Data(s.utf8))) as? [String: Any] ?? [:]
                }
                got.signal()
            }
            XCTAssertEqual(got.wait(timeout: .now() + 5), .success)
            return out
        }
        let hello = try next()
        XCTAssertEqual(hello["pages"] as? Int, 2)
        XCTAssertEqual((hello["notes"] as? [String])?.first, "Hello")
        for m: [String: Any] in [["t": "doc_page", "page": 1], ["t": "pointer", "x": 0.5, "y": 0.5],
                                 ["t": "pointer", "x": 0.6, "y": 0.5], ["t": "pointer", "x": 0.7, "y": 0.5],
                                 ["t": "stop"]] {
            let text = String(decoding: try JSONSerialization.data(withJSONObject: m), as: UTF8.self)
            let done = DispatchSemaphore(value: 0)
            ws.send(.string(text)) { _ in done.signal() }
            done.wait()
        }
        Thread.sleep(forTimeInterval: 0.2)
        XCTAssertEqual(sent.list.map { NSDictionary(dictionary: $0) },
                       [["t": "doc_page", "page": 1], ["t": "pointer", "x": 0.5, "y": 0.5],
                        ["t": "pointer", "x": 0.7, "y": 0.5]].map { NSDictionary(dictionary: $0) },
                       "coalesced; stop is not the page's")
        // The TV's remote turns the page: the presenter view follows; Back on the TV ends it.
        show.heard(["t": "doc_page", "page": 0])
        same(try next(), ["t": "state", "page": 0, "pages": 2, "blank": false])
        show.heard(["t": "blank", "on": true])
        XCTAssertEqual(try next()["blank"] as? Bool, true)
        XCTAssertNotNil(held, "quiet while it is shown")
        show.heard(["t": "stop", "reason": "back"])
        same(try next(), ["t": "ended"])
        XCTAssertNil(held, "do-not-disturb let go when it ends")
        XCTAssertEqual(get(port, path, host: host).0, 404, "the show is over")
        ws.cancel()
    }

    func testANewShowOnTheSameReceiverEndsTheOld() throws {
        let srv = PresentServer()
        try srv.start()
        defer { srv.stop() }
        let make = { Show(receiver: "tv", receiverName: "TV", name: "a.pdf", pdf: self.pdf(), notes: [], ink: false,
                          page: 0, pages: 2, send: { _ in }) }
        let first = make()
        _ = srv.add(first)
        let url = srv.add(make())
        XCTAssertTrue(first.isOver)
        XCTAssertEqual(srv.show(on: "tv")?.url, url)
        srv.end(receiver: "tv")
        XCTAssertNil(srv.show(on: "tv"))
    }

    // MARK: - do not disturb

    final class FakeBanners: Banners {
        var on: Bool?
        var sets = 0
        init(_ on: Bool?) { self.on = on }
        func shown() -> Bool? { on }
        func set(_ on: Bool) {
            self.on = on
            sets += 1
        }
    }

    private func note() -> URL {
        let d = FileManager.default.temporaryDirectory.appendingPathComponent("luch-quiet-\(UUID().uuidString)")
        try? FileManager.default.createDirectory(at: d, withIntermediateDirectories: true)
        return d.appendingPathComponent("restore-banners")
    }

    func testOnHidesBannersForTheSessionAndRestoresThem() {
        let b = FakeBanners(true), n = note()
        let q = Quiet(banners: b, note: n, mode: .on)
        XCTAssertFalse(q.start())
        XCTAssertEqual(b.on, false)
        XCTAssertTrue(FileManager.default.fileExists(atPath: n.path), "written down before the change")
        _ = q.start() // a second session at once
        q.end()
        XCTAssertEqual(b.on, false, "still one session running")
        q.end()
        XCTAssertEqual(b.on, true)
        XCTAssertFalse(FileManager.default.fileExists(atPath: n.path))
    }

    func testAskAsksOnceAndFollowsTheAnswer() {
        let b = FakeBanners(true)
        let q = Quiet(banners: b, note: nil, mode: .ask)
        XCTAssertTrue(q.start())
        XCTAssertEqual(b.on, true, "nothing changes before the answer")
        q.answer(true)
        XCTAssertEqual(b.on, false)
        q.end()
        XCTAssertEqual(b.on, true)
        XCTAssertFalse(q.start(), "asked once while the app runs")
        XCTAssertEqual(b.on, false)
        q.end()
        q.answer(false)
        XCTAssertFalse(q.start())
        XCTAssertEqual(b.on, true)
    }

    func testBannersAlreadyOffOrMissingAreLeftAlone() {
        let b = FakeBanners(false)
        let q = Quiet(banners: b, note: nil, mode: .on)
        _ = q.start()
        q.end()
        XCTAssertEqual(b.on, false, "the person's own Focus stays")
        XCTAssertEqual(b.sets, 0)
        let none = FakeBanners(nil)
        var q2: Quiet? = Quiet(banners: none, note: nil, mode: .on)
        _ = q2?.start()
        q2 = nil
        XCTAssertEqual(none.sets, 0, "no shortcuts: nothing is set")
    }

    func testTurningItOffMidSessionRestoresAtOnceAndAHoldEndsTheSession() {
        let b = FakeBanners(true)
        let q = Quiet(banners: b, note: nil, mode: .on)
        _ = q.start()
        q.setMode(.off)
        XCTAssertEqual(b.on, true)
        q.setMode(.on)
        q.end()
        var asked = false
        var h: AnyObject? = q.hold { asked = true }
        XCTAssertEqual(b.on, false)
        XCTAssertFalse(asked)
        h = nil
        XCTAssertNil(h)
        XCTAssertEqual(b.on, true)
        // Quitting mid-session puts them back; the session's own end later changes nothing.
        let late = q.hold()
        XCTAssertEqual(b.on, false)
        q.endAll()
        XCTAssertEqual(b.on, true)
        withExtendedLifetime(late) {}
    }

    func testACrashLeftBannersOffAndTheNextStartTurnsThemOn() {
        let b = FakeBanners(true), n = note()
        var q: Quiet? = Quiet(banners: b, note: n, mode: .on)
        _ = q?.start()
        // The app dies: no deinit runs. Here a second Quiet reads what the first wrote.
        XCTAssertEqual(b.on, false)
        let again = Quiet(banners: b, note: n, mode: .on)
        again.recover()
        XCTAssertEqual(b.on, true)
        XCTAssertFalse(FileManager.default.fileExists(atPath: n.path))
        withExtendedLifetime(q) {}
        q = nil
    }

    func testFocusStateFromTheFocusDatabase() throws {
        XCTAssertEqual(FocusShortcuts.focusOn(json: Data(#"{"data":[{"storeInvalidationRecords":[]}]}"#.utf8)), false)
        XCTAssertEqual(FocusShortcuts.focusOn(json: Data(#"{"data":[{"storeAssertionRecords":[{"assertionDetails":{}}]}]}"#.utf8)), true)
        XCTAssertNil(FocusShortcuts.focusOn(json: Data("nope".utf8)))
        // The shortcut file is Set Focus with Do Not Disturb, on or off.
        let w = try PropertyListSerialization.propertyList(from: try FocusShortcuts.workflow(on: false), format: nil) as? [String: Any]
        let action = (w?["WFWorkflowActions"] as? [[String: Any]])?.first
        XCTAssertEqual(action?["WFWorkflowActionIdentifier"] as? String, "is.workflow.actions.dnd.set")
        XCTAssertEqual((action?["WFWorkflowActionParameters"] as? [String: Any])?["Enabled"] as? Int, 0)
    }
}
