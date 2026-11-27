// SPDX-License-Identifier: GPL-3.0-or-later
import XCTest
import zlib
@testable import LuchNet
@testable import LuchProto
@testable import LuchReport

/// gzip as java.util.zip.GZIPOutputStream writes it (one member, zlib's deflate).
func gzip(_ data: Data) -> Data {
    var s = z_stream()
    precondition(deflateInit2_(&s, Z_DEFAULT_COMPRESSION, Z_DEFLATED, 16 + MAX_WBITS, 8, Z_DEFAULT_STRATEGY,
                               ZLIB_VERSION, Int32(MemoryLayout<z_stream>.size)) == Z_OK)
    defer { deflateEnd(&s) }
    var input = [UInt8](data)
    var out = [UInt8](repeating: 0, count: Int(deflateBound(&s, uLong(input.count))) + 64)
    let n = input.withUnsafeMutableBufferPointer { inBuf in
        out.withUnsafeMutableBufferPointer { outBuf -> Int in
            s.next_in = inBuf.baseAddress
            s.avail_in = uInt(inBuf.count)
            s.next_out = outBuf.baseAddress
            s.avail_out = uInt(outBuf.count)
            precondition(deflate(&s, Z_FINISH) == Z_STREAM_END)
            return outBuf.count - Int(s.avail_out)
        }
    }
    return Data(out[0..<n])
}

/// "Hide addresses and names": every address and name of a report, the same placeholder each time.
final class RedactorTests: XCTestCase {
    func testIPv4AddressesBecomeExampleOnesStablePerAddress() {
        let r = Redactor()
        XCTAssertEqual(r.redact("from 10.1.2.3 to 172.16.0.9, again 10.1.2.3:47800"),
                       "from 192.0.2.1 to 192.0.2.2, again 192.0.2.1:47800")
        XCTAssertEqual(r.redact("later 172.16.0.9"), "later 192.0.2.2", "the same through the whole report")
    }

    func testAddressesThatNameNobodyStay() {
        let r = Redactor()
        let kept = "127.0.0.1 0.0.0.0 224.0.0.251 255.255.255.255 ::1 :: ff02::fb 00:00:00:00:00:00"
        XCTAssertEqual(r.redact(kept), kept)
    }

    func testWhatOnlyLooksLikeAnAddressStays() {
        let r = Redactor()
        let text = "2026-11-28 10:20:30.123 [  12.345] std::string 1.2.3 v1.2.3.4.5 999.1.1.1 a:b at 12:34"
        XCTAssertEqual(r.redact(text), text)
    }

    func testIPv6HoweverWrittenIsOneAddress() {
        let r = Redactor()
        XCTAssertEqual(r.redact("fe80::1c2b:3a4d:5e6f:7081%en0 and [2001:0db8:85a3:0000:0000:8a2e:0370:7334]:47801"),
                       "2001:db8::1%en0 and [2001:db8::2]:47801")
        XCTAssertEqual(r.redact("FE80:0:0:0:1C2B:3A4D:5E6F:7081"), "2001:db8::1")
        XCTAssertEqual(r.redact("::ffff:10.9.8.7"), "2001:db8::3")
    }

    func testMACAddressesBecomeLocalOnes() {
        let r = Redactor()
        XCTAssertEqual(r.redact("wol aa:bb:cc:dd:ee:01, arp AA-BB-CC-DD-EE-01, other 12:34:56:78:9a:bc"),
                       "wol 02:00:00:00:00:01, arp 02:00:00:00:00:01, other 02:00:00:00:00:02")
    }

    func testNamesBecomeComputerUserAndReceivers() {
        let r = Redactor()
        r.hide("Test-Mac", as: "Computer")
        r.hide("/Users/tester", as: "~")
        r.hide("tester", as: "User")
        XCTAssertEqual(r.receiver(["Living room TV", "living-room"]), "Receiver 1")
        XCTAssertEqual(r.receiver(["Kitchen"]), "Receiver 2")
        XCTAssertEqual(r.receiver(["LIVING ROOM TV"]), "Receiver 1", "a name seen again keeps its number")
        let text = "hello from Test-Mac at /Users/tester/Library (tester) to Living room TV; living-room ok, "
            + "kitchen asked; testers and Kitchenette are other words"
        XCTAssertEqual(r.redact(text),
                       "hello from Computer at ~/Library (User) to Receiver 1; Receiver 1 ok, "
                       + "Receiver 2 asked; testers and Kitchenette are other words")
    }

    func testALongerNameGoesBeforeAShortOneInIt() {
        let r = Redactor()
        r.hide("Box", as: "Receiver 1")
        r.hide("Box Two", as: "Receiver 2")
        XCTAssertEqual(r.redact("Box Two and Box"), "Receiver 2 and Receiver 1")
    }

    func testAOneLetterNameIsLeftAlone() {
        let r = Redactor()
        r.hide("a", as: "User")
        XCTAssertEqual(r.redact("a cat"), "a cat")
    }
}

/// The receiver's log over the control channel (sender side of the "logs" message).
final class ReceiverLogsTests: XCTestCase {
    func testGunzipReadsWhatGzipWrote() {
        let text = String(repeating: "I/luch: stats 60 fps, decode 4.2 ms\n", count: 3000)
        let gz = gzip(Data(text.utf8))
        XCTAssertEqual(gz.prefix(2), Data([0x1f, 0x8b]))
        XCTAssertEqual(ReceiverLogs.gunzip(gz, limit: 1 << 20).map { String(decoding: $0, as: UTF8.self) }, text)
        XCTAssertEqual(ReceiverLogs.gunzip(gz + gzip(Data("more\n".utf8)), limit: 1 << 20)?.count, text.utf8.count + 5,
                       "two members one after another")
    }

    func testBrokenOrHugeGzipIsRefused() {
        let gz = gzip(Data(String(repeating: "x", count: 100_000).utf8))
        XCTAssertNil(ReceiverLogs.gunzip(gz.prefix(gz.count - 9), limit: 1 << 20), "cut short")
        XCTAssertNil(ReceiverLogs.gunzip(Data("not gzip at all, plain text".utf8), limit: 1 << 20))
        var bad = gz
        bad[bad.count - 6] ^= 0xff // the CRC
        XCTAssertNil(ReceiverLogs.gunzip(bad, limit: 1 << 20))
        XCTAssertNil(ReceiverLogs.gunzip(gz, limit: 50_000), "past the limit")
    }

    func testSlicesJoinInTheirOrderCountedFromZeroOrOne() throws {
        let gz = gzip(Data("one\ntwo\nthree\n".utf8))
        let a = gz.prefix(5), b = gz.dropFirst(5)
        for first in [0, 1] {
            var s = ReceiverLogs.Assembler()
            try s.take(["t": "logs", "part": first + 1, "parts": 2, "gz": Data(b).base64EncodedString()])
            XCTAssertFalse(s.isComplete)
            try s.take(["t": "logs", "part": first, "parts": 2, "gz": Data(a).base64EncodedString()])
            XCTAssertTrue(s.isComplete)
            XCTAssertEqual(s.joined, gz)
        }
    }

    func testSlicesOfTwoLogsDoNotMix() throws {
        var s = ReceiverLogs.Assembler()
        try s.take(["t": "logs", "part": 0, "parts": 2, "gz": "AAAA"])
        XCTAssertThrowsError(try s.take(["t": "logs", "part": 1, "parts": 3, "gz": "AAAA"]))
        XCTAssertThrowsError(try s.take(["t": "logs", "part": 9, "parts": 2, "gz": "AAAA"]))
        XCTAssertThrowsError(try s.take(["t": "logs", "part": 1, "parts": 2, "gz": "%%%"]))
    }
}

/// The app's own log: two files of at most `limit`, the newest end taken for a report.
final class RotatingLogTests: XCTestCase {
    func testTheFullFileBecomesTheOlderOne() throws {
        let path = try tempDir().appendingPathComponent("luch.log").path
        let l = RotatingLog(path: path, limit: 100)
        for i in 0..<30 { l.append("line \(i)\n") } // 7 or 8 bytes each
        let current = try String(contentsOfFile: path, encoding: .utf8)
        let older = try String(contentsOfFile: l.older, encoding: .utf8)
        XCTAssertLessThanOrEqual(current.utf8.count, 100)
        XCTAssertLessThanOrEqual(older.utf8.count, 100)
        XCTAssertTrue(current.hasSuffix("line 29\n"))
        XCTAssertFalse(older.contains("line 0\n"), "a third file's worth is gone")

        let tail = RotatingLog.tail(path, maxBytes: 30)
        XCTAssertTrue(tail.hasSuffix("line 29\n"))
        XCTAssertTrue(tail.hasPrefix("line "), "cut at a line's start")
        XCTAssertEqual(RotatingLog.tail(path, maxBytes: 1 << 20), older + current)
    }

    func testAnAppendAfterAReopenGoesOnAtTheEnd() throws {
        let path = try tempDir().appendingPathComponent("luch.log").path
        RotatingLog(path: path).append("a\n")
        RotatingLog(path: path).append("b\n")
        XCTAssertEqual(try String(contentsOfFile: path, encoding: .utf8), "a\nb\n")
    }
}

/// The whole report against receivers made of Swift: one paired that hands over its log, one paired that forgot
/// this Mac, one never paired.
final class ProblemReportTests: XCTestCase {
    var dir: URL!
    var connector: Connector!
    var me: SenderCertificate!

    override func setUpWithError() throws {
        dir = try tempDir()
        let mine = try SenderCertificate.load(dir: dir)
        me = mine
        connector = Connector(dir: dir, certificate: { mine })
    }

    func receiver(_ id: String, _ name: String, tls: UInt16) -> Receiver {
        Receiver(id: id, name: name, model: "Acme Box", address: "127.0.0.1", interfaceName: "lo0", interfaceIndex: 0,
                 port: 1, service: "tv", tlsPort: tls)
    }

    func input(_ receivers: [Receiver], downloads: URL, hide: Bool = true) throws -> ProblemReport.Input {
        let logPath = dir.appendingPathComponent("luch.log").path
        let l = RotatingLog(path: logPath)
        l.append("2026-01-01 10:00:00.000 [   1.000] app: launched, Test-Mac at /Users/tester\n")
        l.append("2026-01-01 10:00:01.000 [   2.000] connected to Living room TV (10.1.2.3 via en0, TLS)\n")
        l.append("2026-01-01 10:00:01.100 [   2.100] receiver: Living room TV (Acme Box) type=tv\n")
        l.append("2026-01-01 10:00:01.200 [   2.200] plan: HEVC 1920x1080@60 audio=true — fastest decoder\n")
        l.append("2026-01-01 10:00:05.000 [   6.000] stats: 60 fps, 9.1 Mbit/s, host 4.0 ms, rtt 3.0 ms\n")
        var i = ProblemReport.Input(what: "HEVC stutters on Living room TV", hide: hide, receivers: receivers,
                                    inUse: [receivers.first?.id ?? ""], otherNames: ["Bedroom"],
                                    me: ("mac-1", "Test-Mac"), app: "Luch for macOS 0.1 (1), luch/0.1 (macos)",
                                    logPath: logPath, downloads: downloads)
        i.connector = connector
        i.reportsBase = dir
        i.machine = ProblemReport.Machine(os: "macOS 99.0", model: "Mac0,0", names: ["Test-Mac"],
                                          user: ["tester", "Test Person"], home: "/Users/tester")
        _ = Reports.store(model: "Acme Box", items: [["id": "3f0c2a8e-9d1b-4c7e-8a55-0b6f2d9e1c47", "kind": "error",
                                                      "what": "no decoder for video/hevc", "time": 1]], base: dir)
        return i
    }

    func unzip(_ file: URL) throws -> URL {
        let out = try tempDir()
        let p = Process()
        p.executableURL = URL(fileURLWithPath: "/usr/bin/ditto")
        p.arguments = ["-x", "-k", file.path, out.path]
        try p.run()
        p.waitUntilExit()
        XCTAssertEqual(p.terminationStatus, 0)
        return out.appendingPathComponent(file.deletingPathExtension().lastPathComponent)
    }

    func testTheReportHoldsEveryLogHidingNamesAndAddresses() throws {
        let tv = try FakeTLSReceiver(dir: try tempDir())
        // Session ids in it: little to pack, so it comes in several slices.
        tv.log = (0..<4000).map { "11-28 10:00:\($0 % 60) I/luch: stats from Test-Mac at 10.1.2.4, session \(UUID())" }
            .joined(separator: "\n") + "\n"
        XCTAssertGreaterThan(tv.logSlices(tv.log!).count, 1)
        tv.trust(me.fp)
        try Pins.pin("tv-1", fp: tv.certificate.fp, name: "Living room TV", dir: dir)
        let forgot = try FakeTLSReceiver(dir: try tempDir())
        forgot.id = "tv-2"
        try Pins.pin("tv-2", fp: forgot.certificate.fp, name: "Kitchen", dir: dir)
        let never = receiver("tv-3", "Hall", tls: 1)

        let downloads = try tempDir()
        let r = try ProblemReport.make(try input([receiver("tv-1", "Living room TV", tls: tv.port),
                                                  receiver("tv-2", "Kitchen", tls: forgot.port), never],
                                                 downloads: downloads))
        XCTAssertEqual(r.file.deletingLastPathComponent().path, downloads.path)
        XCTAssertTrue(r.file.lastPathComponent.hasPrefix("Luch-report-") && r.file.pathExtension == "zip")
        XCTAssertEqual(r.notes.count, 3)
        XCTAssertTrue(r.notes[0].hasPrefix("Receiver 1: log taken"), r.notes[0])
        XCTAssertEqual(r.notes[1], "Receiver 2: asks for pairing again (it forgot this Mac): its log is not asked for")
        XCTAssertEqual(r.notes[2], "Receiver 3: not paired with this Mac: its log is not asked for (that would ask on the TV)")

        // Nothing waits for a code, and the TV that forgot this Mac got no further than its hello.
        XCTAssertEqual(connector.pairing("tv-2")["step"] as? String, "none")
        XCTAssertEqual(forgot.messages.map { $0["t"] as? String }, ["hello"])
        let hello = tv.messages.first { $0["t"] as? String == "hello" }
        XCTAssertEqual(hello?["want"] as? [String], ["logs", "reports"])
        XCTAssertEqual(tv.messages.first { $0["t"] as? String == "logs" }?["max_kb"] as? Int, 1024)

        let out = try unzip(r.file)
        let files = try FileManager.default.contentsOfDirectory(atPath: out.path).sorted()
        XCTAssertEqual(files, ["about.txt", "luch-mac.log", "receiver-1.log", "receiver-reports.json"])
        let all = try files.map { try String(contentsOf: out.appendingPathComponent($0), encoding: .utf8) }
        let receiverLog = all[2]
        XCTAssertEqual(receiverLog.split(separator: "\n").count, 4000, "every line came, in order")
        XCTAssertTrue(receiverLog.hasPrefix("11-28 10:00:0 I/luch: stats from Computer at 192.0.2."), receiverLog.prefix(80).description)
        for (name, text) in zip(files, all) + [("description", r.description), ("issue", r.issueURL)] {
            for secret in ["Living room", "Kitchen", "Hall", "Test-Mac", "tester", "Test Person", "10.1.2.3", "10.1.2.4"] {
                XCTAssertFalse(text.contains(secret), "\(secret) left in \(name)")
            }
        }
        let about = all[0]
        XCTAssertTrue(about.contains("What happened:\nHEVC stutters on Receiver 1"), about)
        XCTAssertTrue(about.contains("1. Receiver 1 (in use): log taken"), about)
        XCTAssertTrue(about.contains("Android SDK 34"), about)
        XCTAssertTrue(about.contains("  plan: HEVC 1920x1080@60 audio=true — fastest decoder\n"), about)
        XCTAssertTrue(about.contains("  stats: 60 fps"), about)
        XCTAssertTrue(about.contains("Crash reports kept: 1"), about)
        XCTAssertTrue(all[1].contains("app: launched, Computer at ~\n"))
        XCTAssertTrue(r.description.contains("What happened: HEVC stutters on Receiver 1"))
        XCTAssertTrue(r.issueURL.hasPrefix(Reports.issues + "?title=Problem%3A%20HEVC%20stutters%20on%20Receiver%201&body="))
        XCTAssertTrue(r.issueURL.contains(Reports.encode(r.file.lastPathComponent)))
    }

    func testKeepNamesLeavesEverythingAsItIs() throws {
        let r = try ProblemReport.make(try input([receiver("tv-3", "Hall", tls: 0)], downloads: try tempDir(), hide: false))
        XCTAssertEqual(r.notes, ["Hall: not paired with this Mac: its log is not asked for (that would ask on the TV)"])
        let about = try String(contentsOf: try unzip(r.file).appendingPathComponent("about.txt"), encoding: .utf8)
        XCTAssertTrue(about.contains("1. Hall (in use)"))
        XCTAssertTrue(about.contains("connected to Living room TV (10.1.2.3"))
        XCTAssertTrue(about.contains("addresses and names kept as they are"))
    }

    func testWhatASentReportCarriesIsTheFilesTexts() throws {
        let r = try ProblemReport.make(try input([receiver("tv-1", "Living room TV", tls: 0), receiver("tv-3", "Hall", tls: 0)],
                                                 downloads: try tempDir()))
        let f = r.facts
        XCTAssertEqual(f.texts.map(\.0), ["about.txt", "luch-mac.log", "receiver-reports.json"], "about.txt first")
        let out = try unzip(r.file)
        for (name, text) in f.texts {
            XCTAssertEqual(text, try String(contentsOf: out.appendingPathComponent(name), encoding: .utf8), name)
        }
        XCTAssertEqual(f.what, "HEVC stutters on Receiver 1")
        XCTAssertEqual(f.receivers, ["Acme Box", "Acme Box"])
        XCTAssertEqual(f.codec, "HEVC")
        XCTAssertEqual(f.os, "macOS 99.0")
        XCTAssertEqual(f.model, "Mac0,0")
    }

    func testAnOlderReceiverIsNotAsked() throws {
        let old = try FakeTLSReceiver(dir: try tempDir())
        old.trust(me.fp)
        try Pins.pin("tv-1", fp: old.certificate.fp, name: "Living room TV", dir: dir)
        let r = try ProblemReport.make(try input([receiver("tv-1", "Living room TV", tls: old.port)], downloads: try tempDir()))
        XCTAssertEqual(r.notes, ["Receiver 1: its Luch cannot hand over its log yet (update it)"])
        XCTAssertNil(old.messages.first { $0["t"] as? String == "logs" }, "not asked: its welcome has no \"logs\"")
    }

    func testANewReportDoesNotTakeTheNameOfAnEarlierOne() throws {
        let d = try tempDir()
        let now = Date(timeIntervalSince1970: 1_800_000_000)
        let first = ProblemReport.uniqueBase(in: d, now: now)
        XCTAssertTrue(first.hasPrefix("Luch-report-20"))
        FileManager.default.createFile(atPath: d.appendingPathComponent(first + ".zip").path, contents: Data())
        XCTAssertEqual(ProblemReport.uniqueBase(in: d, now: now), first + "-2")
    }

    func testTheIssueLinkStaysShortEnough() {
        let long = String(repeating: "the picture froze and the sound went on. ", count: 400)
        let url = ProblemReport.issueURL(what: long, description: long, file: "Luch-report-x.zip")
        XCTAssertLessThanOrEqual(url.utf8.count, 7000)
        XCTAssertTrue(url.contains("Luch-report-x.zip"), "the line asking to attach the file stays")
    }
}
