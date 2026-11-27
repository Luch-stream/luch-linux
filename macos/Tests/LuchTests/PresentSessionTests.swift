// A document presented to a fake receiver: the cast channel carries the presenter view's messages both ways.
// SPDX-License-Identifier: GPL-3.0-or-later
import Darwin
import Foundation
import PDFKit
import XCTest
@testable import LuchCast
@testable import LuchNet
@testable import LuchPresent

/// A receiver that shows documents: answers doc with doc_ready, keeps what it is sent, says what it is told to.
private final class DocReceiver {
    let port: UInt16
    private let listener: Int32
    private var client: Int32 = -1
    private let lock = NSLock()
    private var got: [[String: Any]] = []

    init() {
        let listener = socket(AF_INET, SOCK_STREAM, 0)
        self.listener = listener
        var a = sockaddr_in()
        a.sin_len = UInt8(MemoryLayout<sockaddr_in>.size)
        a.sin_family = sa_family_t(AF_INET)
        a.sin_addr.s_addr = inet_addr("127.0.0.1")
        withUnsafePointer(to: &a) { $0.withMemoryRebound(to: sockaddr.self, capacity: 1) { _ = bind(listener, $0, socklen_t(MemoryLayout<sockaddr_in>.size)) } }
        var len = socklen_t(MemoryLayout<sockaddr_in>.size)
        withUnsafeMutablePointer(to: &a) { $0.withMemoryRebound(to: sockaddr.self, capacity: 1) { _ = getsockname(listener, $0, &len) } }
        port = UInt16(bigEndian: a.sin_port)
        listen(listener, 1)
        Thread.detachNewThread { [self] in
            let c = accept(listener, nil, nil)
            guard c >= 0 else { return }
            lock.lock()
            client = c
            lock.unlock()
            let input = FileHandle(fileDescriptor: c, closeOnDealloc: false)
            var buffer = Data()
            while true {
                let more = input.availableData
                if more.isEmpty { return }
                buffer.append(more)
                while let nl = buffer.firstIndex(of: 10) {
                    let one = buffer[buffer.startIndex..<nl]
                    buffer.removeSubrange(buffer.startIndex...nl)
                    guard let m = (try? JSONSerialization.jsonObject(with: one)) as? [String: Any] else { continue }
                    lock.lock()
                    got.append(m)
                    lock.unlock()
                    switch m["t"] as? String {
                    case "hello": say(["t": "welcome", "v": 1, "name": "Living room TV", "modes": ["ink"]])
                    case "doc": say(["t": "doc_ready", "pages": 2])
                    default: break
                    }
                }
            }
        }
    }

    deinit {
        close(listener)
        if client >= 0 { close(client) }
    }

    func say(_ o: [String: Any]) {
        var d = try! JSONSerialization.data(withJSONObject: o)
        d.append(10)
        lock.lock()
        let c = client
        lock.unlock()
        _ = d.withUnsafeBytes { write(c, $0.baseAddress, d.count) }
    }

    func heard(_ t: String) -> [[String: Any]] {
        lock.lock()
        defer { lock.unlock() }
        return got.filter { $0["t"] as? String == t }
    }

    var receiver: Receiver {
        Receiver(id: "test", name: "Living room TV", model: "", address: "127.0.0.1", interfaceName: "",
                 interfaceIndex: 0, port: port, service: "")
    }
}

final class PresentSessionTests: XCTestCase {
    private func wait(_ what: String, _ ok: () -> Bool) {
        let end = Date().addingTimeInterval(3)
        while !ok() && Date() < end { Thread.sleep(forTimeInterval: 0.02) }
        XCTAssertTrue(ok(), what)
    }

    func testTheCastChannelCarriesThePresenterViewBothWays() throws {
        let tv = DocReceiver()
        let session = try CastSession.open(tv.receiver)
        XCTAssertTrue(session.can("ink"))
        let files = FileServer()
        try files.start(port: 0, address: "127.0.0.1")
        defer { files.stop() }
        let doc = PDFDocument()
        doc.insert(PDFPage(), at: 0)
        doc.insert(PDFPage(), at: 1)
        let pdf = FileManager.default.temporaryDirectory.appendingPathComponent("luch-talk-\(UUID().uuidString).pdf")
        doc.write(to: pdf)
        XCTAssertEqual(try session.cast([pdf.path], options: CastOptions(json: ["title": "talk.pdf"]), server: files), "doc")

        let show = Show(receiver: "test", receiverName: "Living room TV", name: "talk.pdf", pdf: pdf, notes: [],
                        ink: true, page: 0, pages: 2) { [weak session] m in session?.send(m) }
        session.onMessage = { show.heard($0) }
        show.send(["t": "pointer", "x": 0.25, "y": 0.5])
        wait("the pointer reached the receiver") { tv.heard("pointer").count == 1 }
        tv.say(["t": "doc_page", "page": 1])
        tv.say(["t": "blank", "on": true])
        wait("the presenter view follows the TV") { show.hello()["page"] as? Int == 1 && show.hello()["blank"] as? Bool == true }
        tv.say(["t": "stop", "by": "receiver", "reason": "back"])
        wait("Back on the TV ends the show") { show.isOver }
        session.close()
    }

    func testStoppingFromThisMacEndsTheShowToo() throws {
        let tv = DocReceiver()
        let session = try CastSession.open(tv.receiver)
        let show = Show(receiver: "test", receiverName: "TV", name: "a.pdf", pdf: URL(fileURLWithPath: "/nonexistent.pdf"),
                        notes: [], ink: false, page: 0, pages: 1) { _ in }
        session.onMessage = { show.heard($0) }
        session.stop()
        XCTAssertTrue(show.isOver)
        session.close()
    }
}
