// SPDX-License-Identifier: GPL-3.0-or-later
import XCTest
@testable import LuchReport

/// "Send to the Developers": the DSN, the envelope's bytes (the same as luchd's), the event, and the two POSTs as a
/// GlitchTip on loopback gets them.
final class ReportSenderTests: XCTestCase {
    func testADSNNamesTheKeyAndTheEnvelopeEndpoint() {
        let d = ReportSender.DSN("https://0123abcd@reports.example.org/7")
        XCTAssertEqual(d?.key, "0123abcd")
        XCTAssertEqual(d?.url.absoluteString, "https://reports.example.org/api/7/envelope/")
        let p = ReportSender.DSN("http://k:secret@127.0.0.1:8000/glitchtip/12/")
        XCTAssertEqual(p?.key, "k")
        XCTAssertEqual(p?.url.absoluteString, "http://127.0.0.1:8000/glitchtip/api/12/envelope/")
        for bad in ["", "reports.example.org/7", "https://reports.example.org/7", "https://k@reports.example.org",
                    "https://k@reports.example.org/seven", "ftp://k@reports.example.org/7"] {
            XCTAssertNil(ReportSender.DSN(bad), bad)
        }
    }

    func testTheDSNComesFromTheEnvironmentThenInfoPlistAndNoneHidesTheButton() {
        let info = ["LuchReportDSN": "https://a@reports.example.org/1"]
        XCTAssertEqual(ReportSender.configured(info: info, env: [:])?.key, "a")
        XCTAssertEqual(ReportSender.configured(info: info, env: ["LUCH_REPORT_DSN": "https://b@reports.example.org/2"])?.key, "b")
        XCTAssertNil(ReportSender.configured(info: [:], env: [:]))
        XCTAssertNil(ReportSender.configured(info: ["LuchReportDSN": ""], env: [:]))
        XCTAssertNil(ReportSender.configured(info: info, env: ["LUCH_REPORT_DSN": " "]), "the environment can turn it off")
    }

    /// The same bytes as luchd's report_send.rs test.
    func testAnEnvelopeFramesEachItemWithItsLength() {
        let header: [String: Any] = ["event_id": "0123456789abcdef0123456789abcdef",
                                     "sent_at": ReportSender.utc(Date(timeIntervalSince1970: 1_767_323_045))]
        let b = ReportSender.envelope(header: header, items: [
            (["type": "event"], Data(#"{"a":1}"#.utf8)),
            (["type": "attachment", "filename": "r.zip"], Data([0x50, 0x4B, 0x03, 0x04, 0x0A, 0x0A])),
        ])
        let expected = Data(("{\"event_id\":\"0123456789abcdef0123456789abcdef\",\"sent_at\":\"2026-01-02T03:04:05Z\"}\n"
            + "{\"length\":7,\"type\":\"event\"}\n{\"a\":1}\n"
            + "{\"filename\":\"r.zip\",\"length\":6,\"type\":\"attachment\"}\n").utf8) + Data([0x50, 0x4B, 0x03, 0x04, 0x0A, 0x0A, 0x0A])
        XCTAssertEqual(b, expected, String(decoding: b, as: UTF8.self))
    }

    func testTheNumberIsTheIdsStart() {
        XCTAssertEqual(ReportSender.number("7f3c0a1b2c3d4e5f6a7b8c9d0e1f2a3b"), "LUCH-7F3C0A1B")
    }

    func facts() -> ProblemReport.Facts {
        ProblemReport.Facts(what: "HEVC stutters on Receiver 1\nsecond line",
                            texts: [("about.txt", "Luch problem report\n"), ("luch-mac.log", String(repeating: "x", count: 100_000))],
                            app: "Luch for macOS 0.1 (1), luch/0.1 (macos)", version: "0.1", os: "macOS 99.0",
                            model: "Mac0,0", receivers: ["Acme Box"], codec: "HEVC")
    }

    func testTheEventIsTitledByWhatHappenedAndHoldsTheTails() {
        let e = ReportSender.event(facts(), id: "0123456789abcdef0123456789abcdef", now: Date(timeIntervalSince1970: 0))
        XCTAssertEqual(e["message"] as? String, "Problem report: HEVC stutters on Receiver 1")
        XCTAssertEqual(e["level"] as? String, "info")
        XCTAssertEqual(e["platform"] as? String, "other")
        XCTAssertEqual(e["release"] as? String, "luch-macos@0.1")
        XCTAssertEqual(e["tags"] as? [String: String], ["app": "Luch for macOS 0.1 (1), luch/0.1 (macos)", "os": "macOS 99.0",
                                                         "receivers": "Acme Box", "codec": "HEVC"])
        let contexts = e["contexts"] as? [String: [String: String]]
        XCTAssertEqual(contexts?["os"], ["name": "macOS", "version": "99.0"])
        XCTAssertEqual(contexts?["device"], ["model": "Mac0,0"])
        let extra = e["extra"] as? [String: String] ?? [:]
        XCTAssertEqual(extra["what happened"], "HEVC stutters on Receiver 1\nsecond line")
        XCTAssertEqual(extra["about.txt"], "Luch problem report\n")
        XCTAssertEqual(extra["luch-mac.log"]?.utf8.count, ReportSender.each)
    }

    func testManyLogsKeepTheEventUnderItsLimit() throws {
        var f = facts()
        f.what = ""
        f.texts = (0..<30).map { ("receiver-\($0).log", String(repeating: "é", count: 50_000)) }
        let e = ReportSender.event(f, id: "0123456789abcdef0123456789abcdef", now: Date())
        XCTAssertLessThan(try JSONSerialization.data(withJSONObject: e).count, 800 * 1024)
        XCTAssertEqual(e["message"] as? String, "Problem report")
        let extra = e["extra"] as? [String: String] ?? [:]
        XCTAssertLessThanOrEqual(extra["receiver-0.log"]?.utf8.count ?? .max, ReportSender.each)
        XCTAssertEqual(extra["receiver-0.log"]?.first, "é", "cut at a character's start")
        XCTAssertEqual(extra["receiver-29.log"], "", "nothing left for the last ones")
    }

    func testARefusalSaysWhy() {
        XCTAssertNil(ReportSender.refusal(status: 200, body: Data(#"{"id":"x"}"#.utf8), retryAfter: nil))
        XCTAssertEqual(ReportSender.refusal(status: 429, body: Data(), retryAfter: "120"),
                       "the developers' server takes no more reports now, try again in 2 min")
        XCTAssertEqual(ReportSender.refusal(status: 429, body: Data(), retryAfter: nil),
                       "the developers' server takes no more reports now, try again later")
        XCTAssertEqual(ReportSender.refusal(status: 403, body: Data(#"{"detail":"bad key"}"#.utf8), retryAfter: nil),
                       "the developers' server did not take this build's key (HTTP 403)")
        XCTAssertEqual(ReportSender.refusal(status: 400, body: Data(#"{"detail":"invalid envelope"}"#.utf8), retryAfter: nil),
                       "the developers' server answered HTTP 400: invalid envelope")
        XCTAssertEqual(ReportSender.refusal(status: 502, body: Data(), retryAfter: nil),
                       "the developers' server answered HTTP 502")
    }

    /// An envelope's lines: (header, [(item header, payload)]), the payloads taken by their `length`.
    func unframe(_ b: Data) throws -> ([String: Any], [([String: Any], Data)]) {
        let bytes = [UInt8](b)
        var i = bytes.firstIndex(of: 0x0A)!
        let header = try JSONSerialization.jsonObject(with: Data(bytes[..<i])) as! [String: Any]
        var items: [([String: Any], Data)] = []
        i += 1
        while i < bytes.count {
            let nl = bytes[i...].firstIndex(of: 0x0A)!
            let h = try JSONSerialization.jsonObject(with: Data(bytes[i..<nl])) as! [String: Any]
            let len = h["length"] as! Int
            items.append((h, Data(bytes[(nl + 1)..<(nl + 1 + len)])))
            XCTAssertEqual(bytes[nl + 1 + len], 0x0A, "a payload ends with a newline")
            i = nl + 2 + len
        }
        return (header, items)
    }

    func report(_ zip: Data) throws -> ProblemReport.Result {
        let file = try tempDir().appendingPathComponent("Luch-report-2026-01-02-0304.zip")
        try zip.write(to: file)
        return ProblemReport.Result(file: file, description: "", issueURL: "", notes: [], facts: facts())
    }

    func testAReportGoesAsAnEventThenItsZip() throws {
        var got: [TestRequest] = []
        let lock = NSLock()
        let server = TestHTTPServer { r in
            lock.lock()
            got.append(r)
            lock.unlock()
            return TestResponse(status: 200, body: Data(#"{"id":"x"}"#.utf8))
        }
        let zip = Data("PK\u{3}\u{4} zip bytes".utf8)
        let dsn = try XCTUnwrap(ReportSender.DSN("http://pubkey@127.0.0.1:\(server.port)/3"))
        var steps: [Double] = []
        let done = expectation(description: "sent")
        var result: Result<ReportSender.Sent, ReportError>?
        let upload = ReportSender.Upload(try report(zip), to: dsn)
        upload.start(progress: { p in lock.lock(); steps.append(p); lock.unlock() }) { r in
            result = r
            done.fulfill()
        }
        wait(for: [done], timeout: 10)
        let sent = try result!.get()
        XCTAssertNil(sent.attachError)
        XCTAssertEqual(got.count, 2)
        let ev = got[0]
        XCTAssertEqual(ev.method, "POST")
        XCTAssertEqual(ev.path, "/api/3/envelope/")
        XCTAssertEqual(ev.headers["x-sentry-auth"], "Sentry sentry_version=7, sentry_key=pubkey, sentry_client=luch/0.1")
        XCTAssertEqual(ev.headers["content-type"], "application/x-sentry-envelope")
        let (header, items) = try unframe(ev.body)
        let id = try XCTUnwrap(header["event_id"] as? String)
        XCTAssertEqual(id.count, 32)
        XCTAssertTrue(id.allSatisfy(\.isHexDigit))
        XCTAssertTrue((header["sent_at"] as? String)?.hasSuffix("Z") == true)
        XCTAssertEqual(items.count, 1)
        XCTAssertEqual(items[0].0["type"] as? String, "event")
        let e = try JSONSerialization.jsonObject(with: items[0].1) as? [String: Any]
        XCTAssertEqual(e?["event_id"] as? String, id)
        XCTAssertEqual(e?["message"] as? String, "Problem report: HEVC stutters on Receiver 1")

        let (h2, att) = try unframe(got[1].body)
        XCTAssertEqual(h2["event_id"] as? String, id, "the zip goes to the same event")
        XCTAssertEqual(att[0].0 as NSDictionary, ["type": "attachment", "length": zip.count,
                                                  "filename": "Luch-report-2026-01-02-0304.zip",
                                                  "content_type": "application/zip",
                                                  "attachment_type": "event.attachment"] as NSDictionary)
        XCTAssertEqual(att[0].1, zip)
        XCTAssertEqual(sent.number, ReportSender.number(id))
        XCTAssertEqual(sent.eventID, id)
        XCTAssertEqual(steps.last, 1, "the bar ends full")
    }

    func testARefusedZipStillSendsTheReportARefusedEventDoesNot() throws {
        var answers = [TestResponse(status: 200, body: Data("{}".utf8)),
                       TestResponse(status: 400, body: Data(#"{"detail":"unsupported item"}"#.utf8)),
                       TestResponse(status: 429, headers: [("Retry-After", "30")], body: Data("{}".utf8))]
        let lock = NSLock()
        let server = TestHTTPServer { _ in
            lock.lock()
            defer { lock.unlock() }
            return answers.removeFirst()
        }
        let dsn = try XCTUnwrap(ReportSender.DSN("http://k@127.0.0.1:\(server.port)/3"))
        let r = try report(Data("PK\u{3}\u{4}".utf8))
        let first = try ReportSender.send(r, to: dsn).get()
        XCTAssertEqual(first.attachError, "the developers' server answered HTTP 400: unsupported item")
        guard case let .failure(e) = ReportSender.send(r, to: dsn) else { return XCTFail("a refused event is not sent") }
        XCTAssertEqual(e.localizedDescription, "the developers' server takes no more reports now, try again in 30 s")
    }

    func testNoServerIsTold() throws {
        let dsn = try XCTUnwrap(ReportSender.DSN("http://k@127.0.0.1:9/3"))
        guard case let .failure(e) = ReportSender.send(try report(Data("PK".utf8)), to: dsn) else {
            return XCTFail("nothing answered")
        }
        XCTAssertTrue(e.localizedDescription.hasPrefix("cannot reach the developers' server"), e.localizedDescription)
    }
}
