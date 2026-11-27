// TVs without Luch against the fakes (FakeDLNA.swift, FakeCast.swift): found the way real ones are, fed real files
// through the FileServer, driven through the session and then through the local API's commands. The cases and the
// test vectors are luchd's (luch-linux luchd/src/tv/tests.rs), so both senders agree.
// SPDX-License-Identifier: GPL-3.0-or-later
import Foundation
import XCTest
@testable import LuchCast
@testable import LuchTV

/// The states a session told, read in order like a channel.
final class States {
    private let cond = NSCondition()
    private var all: [[String: Any]] = []
    private var read = 0

    var sink: StateSink {
        { [weak self] _, v in self?.add(v) }
    }

    func add(_ v: [String: Any]) {
        cond.lock()
        all.append(v)
        cond.broadcast()
        cond.unlock()
    }

    /// Waits for a state matching `want`, failing on any end that does not.
    @discardableResult
    func until(_ what: String, timeout: Double = 15, file: StaticString = #filePath, line: UInt = #line,
               _ want: ([String: Any]) -> Bool) -> [String: Any] {
        let deadline = Date().addingTimeInterval(timeout)
        cond.lock()
        defer { cond.unlock() }
        var seen: [[String: Any]] = []
        while true {
            while read < all.count {
                let v = all[read]
                read += 1
                if want(v) { return v }
                if v["state"] as? String == "stopped" {
                    XCTFail("stopped early waiting for \(what): \(v)", file: file, line: line)
                    return v
                }
                seen.append(v)
            }
            if !cond.wait(until: deadline) {
                XCTFail("no \(what) in time; told instead: \(seen)", file: file, line: line)
                return [:]
            }
        }
    }

    @discardableResult
    func until(_ state: String, timeout: Double = 15, file: StaticString = #filePath, line: UInt = #line) -> [String: Any] {
        until(state, timeout: timeout, file: file, line: line) { $0["state"] as? String == state }
    }
}

final class TVTests: XCTestCase {
    private func files() throws -> FileServer {
        let s = FileServer()
        try s.start(port: 0, address: "127.0.0.1")
        return s
    }

    private func temp(_ name: String, _ data: Data) -> URL {
        let dir = FileManager.default.temporaryDirectory.appendingPathComponent("luch-tv-\(UUID().uuidString)")
        try? FileManager.default.createDirectory(at: dir, withIntermediateDirectories: true)
        let u = dir.appendingPathComponent(name)
        try? data.write(to: u)
        return u
    }

    private func videoAndSubs() -> [String] {
        let v = temp("Holiday 2026.mp4", Data(repeating: 7, count: 50_000))
        let s = v.deletingLastPathComponent().appendingPathComponent("Holiday 2026.en.srt")
        try? Data("1\r\n00:00:01,000 --> 00:00:02,500\r\nHello\r\n".utf8).write(to: s)
        return [v.path, s.path]
    }

    private func media(_ items: [String], _ srv: FileServer, _ o: CastOptions = CastOptions()) throws -> Media {
        try Media(message: Cast.build(items, o: o, server: srv, client: "127.0.0.1", me: "127.0.0.1"))
    }

    private func castDevice(_ port: UInt16) -> Device {
        CastDNS.device(address: "127.0.0.1", port: port,
                       txt: ["id": "1c4f3a2b9d8e7f60a1b2c3d4e5f60718", "md": "Chromecast", "fn": "Living room TV", "ca": "4101"])!
    }

    private func dlnaDevice(_ fake: FakeRenderer) throws -> Device {
        try XCTUnwrap(try SSDP.describe(fake.location).get())
    }

    // MARK: - the wire and the rules

    /// The bytes cast_channel.proto gives for a CONNECT, worked out by hand from the field numbers (luchd's vector).
    func testACastMessageIsTheBytesTheProtoGives() throws {
        let m = CastV2.Message("sender-0", "receiver-0", CastV2.nsConnection, json: ["type": "CONNECT"])
        let b = m.encode()
        var want = Data([0x08, 0x00, 0x12, 8]) + Data("sender-0".utf8)
        want += Data([0x1a, 10]) + Data("receiver-0".utf8)
        want += Data([0x22, UInt8(CastV2.nsConnection.utf8.count)]) + Data(CastV2.nsConnection.utf8)
        want += Data([0x28, 0x00, 0x32, 18]) + Data(#"{"type":"CONNECT"}"#.utf8)
        XCTAssertEqual(b, want)
        XCTAssertEqual(try CastV2.Message.decode(b), m)
        var bin = m
        bin.payload = .binary(Data(count: 300))
        XCTAssertEqual(try CastV2.Message.decode(bin.encode()), bin)
        XCTAssertThrowsError(try CastV2.Message.decode(Data([0x12, 50, UInt8(ascii: "x")])))
    }

    func testALoadNamesTheMediaAndItsSubtitles() {
        var m = Media()
        m.url = "http://192.168.1.20:47810/t/a.mp4"
        m.mime = "video/mp4"
        m.title = "Holiday"
        m.start = 12
        m.subs = [Sub(url: "http://192.168.1.20:47810/u/a.en.srt", mime: "text/vtt", lang: "en")]
        let l = CastV2.load(m, session: "s-1")
        let media = l["media"] as! [String: Any]
        XCTAssertEqual(l["type"] as? String, "LOAD")
        XCTAssertEqual(l["sessionId"] as? String, "s-1")
        XCTAssertEqual(media["contentId"] as? String, m.url)
        XCTAssertEqual(media["streamType"] as? String, "BUFFERED")
        XCTAssertEqual((media["tracks"] as! [[String: Any]])[0]["trackContentType"] as? String, "text/vtt")
        XCTAssertEqual(l["activeTrackIds"] as? [Int], [1])
        XCTAssertEqual(l["currentTime"] as? Double, 12)
        XCTAssertEqual(l["autoplay"] as? Bool, true)
    }

    func testTheEnvelopeIsTheOneUDASpells() {
        let e = DLNA.envelope("urn:schemas-upnp-org:service:AVTransport:1", "Seek",
                              [("InstanceID", "0"), ("Unit", "REL_TIME"), ("Target", "0:01:30")])
        XCTAssertTrue(e.contains("<u:Seek xmlns:u=\"urn:schemas-upnp-org:service:AVTransport:1\"><InstanceID>0</InstanceID><Unit>REL_TIME</Unit><Target>0:01:30</Target></u:Seek>"), e)
        XCTAssertTrue(e.contains("s:encodingStyle=\"http://schemas.xmlsoap.org/soap/encoding/\""))
    }

    func testDIDLCarriesTypeFeaturesAndSubtitles() {
        var m = Media()
        m.url = "http://192.168.1.20:47810/t/Film & Co.mkv"
        m.mime = "video/x-matroska"
        m.title = "Film & Co"
        m.ours = true
        m.subs = [Sub(url: "http://192.168.1.20:47810/u/film.en.srt", mime: "application/x-subrip", lang: "en")]
        let d = DLNA.didl(m)
        XCTAssertTrue(d.contains("<dc:title>Film &amp; Co</dc:title>"))
        XCTAssertTrue(d.contains("<upnp:class>object.item.videoItem</upnp:class>"))
        XCTAssertTrue(d.contains("protocolInfo=\"http-get:*:video/x-matroska:\(FileServer.dlnaStream)\""))
        XCTAssertTrue(d.contains("<sec:CaptionInfoEx sec:type=\"srt\">http://192.168.1.20:47810/u/film.en.srt</sec:CaptionInfoEx>"))
        var photo = Media()
        photo.url = "http://x/a.jpg"
        photo.mime = "image/jpeg"
        photo.photo = true
        let p = DLNA.didl(photo)
        XCTAssertTrue(p.contains("object.item.imageItem.photo") && p.contains(FileServer.dlnaPhoto))
        var song = Media()
        song.url = "http://x/a.mp3"
        song.mime = "audio/mpeg"
        let s = DLNA.didl(song)
        XCTAssertTrue(s.contains("object.item.audioItem.musicTrack") && s.contains("audio/mpeg:*\""))
    }

    func testTimesGoBothWays() {
        XCTAssertEqual(hms(3723.9), "1:02:03")
        XCTAssertEqual(parseHMS("1:02:03"), 3723)
        XCTAssertEqual(parseHMS("00:00:05.500"), 5.5)
        XCTAssertEqual(parseHMS("0:00:01.1/2"), 1.5)
        XCTAssertNil(parseHMS("NOT_IMPLEMENTED"))
        XCTAssertNil(parseHMS(""))
    }

    func testXMLElementsByLocalName() {
        let x = """
        <?xml version="1.0"?><s:Envelope xmlns:s="x"><s:Body><u:GetPositionInfoResponse>
            <Track>1</Track><RelTime>0:01:02</RelTime><TrackURI/><TrackMetaData>&lt;DIDL-Lite&gt;</TrackMetaData>
            </u:GetPositionInfoResponse></s:Body></s:Envelope>
        """
        XCTAssertEqual(XML.text(x, "RelTime"), "0:01:02")
        XCTAssertEqual(XML.text(x, "TrackURI"), "")
        XCTAssertEqual(XML.text(x, "TrackMetaData"), "<DIDL-Lite>")
        XCTAssertNil(XML.text(x, "Missing"))
        XCTAssertEqual(XML.all(x, "Body").count, 1)
        // "Track" must not match "TrackURI" or "TrackMetaData".
        XCTAssertEqual(XML.all(x, "Track"), ["1"])
        let r = #"<res protocolInfo="http-get:*:video/mp4:*" size='12'>u</res>"#
        XCTAssertEqual(XML.attr(r, "res", "protocolInfo"), "http-get:*:video/mp4:*")
        XCTAssertEqual(XML.attr(r, "res", "size"), "12")
        XCTAssertEqual(XML.unescape("a &amp; b &#x41;&#66; &bogus; &"), "a & b AB &bogus; &")
        XCTAssertEqual(XML.escape("<a href=\"x\">&</a>"), "&lt;a href=&quot;x&quot;&gt;&amp;&lt;/a&gt;")
        XCTAssertEqual(XML.unescape(XML.escape("Tom & Jerry <1>")), "Tom & Jerry <1>")
    }

    func testSSDPAnswersAndNotifiesParseOthersDoNot() {
        let a = "HTTP/1.1 200 OK\r\nCACHE-CONTROL: max-age=1800\r\nLocation: http://192.168.1.40:9197/dmr\r\nST: urn:schemas-upnp-org:device:MediaRenderer:1\r\nUSN: uuid:abc::urn:schemas-upnp-org:device:MediaRenderer:1\r\n\r\n"
        let h = SSDP.parse(a)!
        XCTAssertEqual(h["location"], "http://192.168.1.40:9197/dmr")
        XCTAssertTrue(h["usn"]!.hasPrefix("uuid:abc"))
        XCTAssertNil(SSDP.parse("M-SEARCH * HTTP/1.1\r\nST: ssdp:all\r\n\r\n"))
        let s = SSDP.mSearch(SSDP.renderer, mx: 2)
        XCTAssertTrue(s.hasPrefix("M-SEARCH * HTTP/1.1\r\n"))
        XCTAssertTrue(s.contains("MAN: \"ssdp:discover\"\r\n") && s.hasSuffix("\r\n\r\n"))
    }

    func testADescriptionGivesAbsoluteControlURLs() {
        let desc = fakeDescription.replacingOccurrences(of: "{UDN}", with: "uuid:1234-5678")
        let d = SSDP.device(location: "http://192.168.1.40:9197/dmr", description: desc)!
        XCTAssertEqual(d.id, "dlna:1234-5678")
        XCTAssertEqual(d.name, "Living room TV")
        XCTAssertEqual(d.model, "Example Corp TV 55")
        XCTAssertEqual(d.address, "192.168.1.40")
        XCTAssertEqual(d.port, 9197)
        guard case let .dlna(av, _, rc) = d.control else { return XCTFail() }
        XCTAssertEqual(av, "http://192.168.1.40:9197/upnp/control/AVTransport1")
        XCTAssertEqual(rc?.url, "http://192.168.1.40:9197/upnp/control/RenderingControl1")
        // No AVTransport: a media server or a speaker group, not for us.
        XCTAssertNil(SSDP.device(location: "http://192.168.1.40/", description: "<root><device><UDN>uuid:x</UDN></device></root>"))
    }

    func testATXTRecordMakesAChromecastAndSpeakersAreLeftOut() {
        var txt = ["id": "1c4f3a2b9d8e7f60a1b2c3d4e5f60718", "cd": "0A1B2C3D4E5F", "md": "Chromecast", "fn": "Living room TV",
                   "ca": "465413", "ve": "05"]
        let d = CastDNS.device(address: "192.168.1.41", port: 8009, txt: txt)!
        XCTAssertEqual(d.id, "cast:1c4f3a2b9d8e7f60a1b2c3d4e5f60718")
        XCTAssertEqual(d.name, "Living room TV")
        XCTAssertEqual(d.model, "Chromecast")
        XCTAssertEqual(d.address, "192.168.1.41")
        XCTAssertEqual(d.port, 8009)
        txt["ca"] = "199172" // audio, no video bit
        XCTAssertNil(CastDNS.device(address: "192.168.1.42", port: 8009, txt: txt))
        txt["ca"] = nil
        txt["id"] = nil
        XCTAssertNil(CastDNS.device(address: "192.168.1.41", port: 8009, txt: txt))
    }

    func testOneRowPerTVWithTheBestWayFirst() {
        func e(_ k: TVKind, _ id: String, _ address: String) -> Entry {
            Entry(kind: k, id: id, name: "TV \(id)", model: "", address: address, port: 1)
        }
        let groups = group([e(.dlna, "dlna:a", "192.168.1.40"), e(.cast, "cast:a", "192.168.1.40"), e(.luch, "luch-a", "192.168.1.40"),
                            // A speaker group at the TV's address is another Chromecast: its own row.
                            e(.cast, "cast:group", "192.168.1.40"), e(.dlna, "dlna:b", "192.168.1.41")])
        XCTAssertEqual(groups.count, 3, "\(groups)")
        let tv = groups.first { $0[0].id == "luch-a" }!
        XCTAssertEqual(tv.map(\.kind), [.luch, .cast, .dlna])
        XCTAssertEqual(pick(groups, id: "luch-a", forced: nil)?.id, "luch-a")
        // The hidden choice: any id of the TV, any of its ways.
        XCTAssertEqual(pick(groups, id: "dlna:a", forced: .cast)?.id, "cast:a")
        XCTAssertEqual(pick(groups, id: "luch-a", forced: .dlna)?.id, "dlna:a")
        XCTAssertNil(pick(groups, id: "dlna:b", forced: .luch))
        XCTAssertNil(pick(groups, id: "nobody", forced: nil))
        XCTAssertEqual(TVKind.of("cast:1c4f"), .cast)
        XCTAssertEqual(TVKind.of("dlna:5f1c"), .dlna)
        XCTAssertEqual(TVKind.of("3f0c2a8e-9d1b"), .luch)
        XCTAssertFalse(TVKind.dlna.can.contains("mirror"))
    }

    func testWhatATVIsGivenComesFromTheLuchMessage() throws {
        let srv = try files()
        let m = try media(videoAndSubs(), srv)
        XCTAssertEqual(m.mime, "video/mp4")
        XCTAssertEqual(m.title, "Holiday 2026")
        XCTAssertTrue(m.ours && !m.photo)
        XCTAssertEqual(m.subs.count, 1)
        XCTAssertEqual(m.subs[0].lang, "en")
        XCTAssertTrue(try media([temp("a.jpg", Data("jpeg".utf8)).path], srv).photo)
        XCTAssertThrowsError(try Media(message: ["t": "open_app", "app": "youtube", "id": "x"]))
        XCTAssertThrowsError(try Media(message: ["t": "doc"]))
    }

    func testStatesAreToldOnChangeNotOnEveryPoll() {
        var got: [[String: Any]] = []
        let s = TVSession(device: castDevice(1)) { _, v in got.append(v) }
        let t = s.tell
        t.tell("loading", pos: 0, dur: nil, rate: 1)
        t.tell("playing", pos: 0, dur: 60, rate: 1)
        t.tell("playing", pos: 0.3, dur: 60, rate: 1) // just time going on
        t.tell("playing", pos: 30, dur: 60, rate: 1) // a seek on the TV
        t.asked()
        t.tell("paused", pos: 30, dur: 60, rate: 1)
        t.asked()
        t.tell("paused", pos: 30, dur: 60, rate: 1)
        t.stopped("receiver")
        let short = got.map { "\($0["state"]!) \(($0["pos"] as? Double) ?? -1) \($0["by"] as? String ?? "")" }
        XCTAssertEqual(short, ["loading 0.0 receiver", "playing 0.0 receiver", "playing 30.0 receiver", "paused 30.0 sender",
                               "paused 30.0 sender", "stopped -1.0 "])
        XCTAssertEqual(got[1]["dur"] as? Double, 60)
        XCTAssertEqual(got[0]["live"] as? Bool, false)
        XCTAssertEqual(got[5]["reason"] as? String, "receiver")
    }

    /// What TVs without Luch ask of the file server: DLNA's content features, CORS for a Chromecast's page (and its
    /// preflight), WebVTT made of SubRip, a header added for one link.
    func testTVsGetDLNAFeaturesCORSAndWebVTT() throws {
        let srv = try files()
        let v = temp("a.mp4", Data("0123456789".utf8))
        let t = srv.share(.file(v), client: "127.0.0.1", mime: "video/mp4")
        let url = srv.url(host: "127.0.0.1", token: t, name: "a.mp4")
        srv.addHeader(url: url, name: "CaptionInfo.sec", value: "http://127.0.0.1/s.srt")
        var r = testFetch(url, headers: ["getcontentFeatures.dlna.org": "1", "transferMode.dlna.org": "Streaming"])
        XCTAssertEqual(r.1["contentfeatures.dlna.org"], FileServer.dlnaStream)
        XCTAssertEqual(r.1["transfermode.dlna.org"], "Streaming")
        XCTAssertEqual(r.1["captioninfo.sec"], "http://127.0.0.1/s.srt")
        XCTAssertNil(r.1["access-control-allow-origin"], "no CORS unless a page asks")
        r = testFetch(url, headers: ["Origin": "https://www.gstatic.com"])
        XCTAssertEqual(r.1["access-control-allow-origin"], "*")
        r = testFetch(url, headers: ["Origin": "https://www.gstatic.com", "Access-Control-Request-Headers": "range"], method: "OPTIONS")
        XCTAssertEqual(r.0, 204)
        XCTAssertTrue(r.1["access-control-allow-headers"]?.contains("Range") == true)

        let srt = temp("a.en.srt", Data("\u{feff}1\r\n00:00:01,000 --> 00:00:02,500\r\nHello, world\r\n".utf8))
        let st = srv.share(.file(srt), client: "127.0.0.1", mime: "application/x-subrip")
        let surl = srv.url(host: "127.0.0.1", token: st, name: "a.en.srt")
        XCTAssertTrue(srv.asVTT(url: surl))
        XCTAssertFalse(srv.asVTT(url: "http://127.0.0.1:1/nope/x.srt"))
        r = testFetch(surl)
        XCTAssertEqual(r.0, 200)
        XCTAssertTrue(r.1["content-type"]?.contains("text/vtt") == true)
        XCTAssertEqual(String(decoding: r.2, as: UTF8.self), "WEBVTT\n\n1\n00:00:01.000 --> 00:00:02.500\nHello, world\n")
    }

    // MARK: - against the fakes

    /// Found over real SSDP multicast (TTL 0 on loopback: only this machine hears it), then played, paused, sought,
    /// its volume set, played to the end.
    func testADLNATVIsFoundAndPlaysToTheEnd() throws {
        let fake = FakeRenderer(dur: 4)
        let answers = try SSDP.search(SSDP.Target(port: fake.ssdpPort, ttl: 0, interfaces: ["127.0.0.1"]), wait: 1.5)
        let a = try XCTUnwrap(answers.first { $0.usn.hasPrefix(fake.udn) }, "the fake renderer did not answer the multicast search: \(answers)")
        let d = try XCTUnwrap(try SSDP.describe(a.location).get())
        XCTAssertEqual(d.kind, .dlna)
        XCTAssertEqual(d.name, "Living room TV")

        let srv = try files()
        let m = try media(videoAndSubs(), srv)
        DLNA.prepare(srv, m)
        let states = States()
        let s = try play(d, m, onState: states.sink)
        states.until("playing")
        fake.with { av in
            let f = av.fetches[0]
            XCTAssertEqual(f.status, 206, "a TV asks for a range: \(f)")
            XCTAssertEqual(f.headers["contentfeatures.dlna.org"], FileServer.dlnaStream)
            XCTAssertEqual(f.headers["transfermode.dlna.org"], "Streaming")
            XCTAssertEqual(f.headers["captioninfo.sec"], m.subs[0].url)
            XCTAssertEqual(f.headers["content-range"], "bytes 0-49999/50000")
            XCTAssertTrue(av.meta.contains("sec:CaptionInfoEx"))
            XCTAssertEqual(av.uri, m.url)
        }
        XCTAssertEqual(Array(fake.actions.prefix(3)), ["Stop", "SetAVTransportURI", "Play"])

        XCTAssertNoThrow(try s.ask(.cmd(["do": "pause"])).get())
        XCTAssertEqual(states.until("paused")["by"] as? String, "sender")
        XCTAssertNoThrow(try s.ask(.cmd(["do": "seek", "pos": 2])).get())
        states.until("paused at 2") { $0["state"] as? String == "paused" && $0["pos"] as? Double == 2 }
        XCTAssertThrowsError(try s.ask(.cmd(["do": "rate", "rate": 1.5])).get()) {
            XCTAssertEqual($0.localizedDescription, "this TV plays at one speed only")
        }
        XCTAssertEqual(try s.ask(.volume(["t": "volume", "level": 40])).get() as NSDictionary, ["level": 40, "muted": false])
        XCTAssertEqual(try s.ask(.volume(["t": "volume", "step": 1])).get() as NSDictionary, ["level": 45, "muted": false])
        XCTAssertEqual(try s.ask(.volume(["t": "volume", "muted": true])).get() as NSDictionary, ["level": 45, "muted": true])
        XCTAssertNoThrow(try s.ask(.cmd(["do": "play"])).get())
        states.until("ended")
        XCTAssertEqual(states.until("stopped")["reason"] as? String, "ended")
    }

    func testADLNATVStoppedOnItsRemoteOrByUs() throws {
        let fake = FakeRenderer(dur: 600)
        let d = try dlnaDevice(fake)
        let srv = try files()
        var o = CastOptions()
        o.start = 30
        let item = Array(videoAndSubs().prefix(1))
        var states = States()
        _ = try play(d, try media(item, srv, o), onState: states.sink)
        // The start position is sought once it plays.
        states.until("playing from 30") { $0["state"] as? String == "playing" && ($0["pos"] as? Double ?? 0) >= 30 }
        fake.pressStop()
        XCTAssertEqual(states.until("stopped")["reason"] as? String, "receiver")

        states = States()
        let s = try play(d, try media(item, srv, o), onState: states.sink)
        states.until("playing")
        XCTAssertNoThrow(try s.ask(.stop).get())
        XCTAssertEqual(states.until("stopped")["reason"] as? String, "sender")
        XCTAssertEqual(fake.with { $0.state }, "STOPPED")
    }

    func testAChromecastPlaysWithSubtitlesAndObeys() throws {
        let fake = try FakeCast(dur: 30)
        let d = castDevice(fake.port)
        let srv = try files()
        var m = try media(videoAndSubs(), srv)
        CastV2.prepare(srv, &m)
        XCTAssertEqual(m.subs[0].mime, "text/vtt", "SubRip is served as WebVTT to a Chromecast")
        let states = States()
        let s = try play(d, m, onState: states.sink)
        states.until("playing")
        fake.with { tv in
            let log = tv.log.joined(separator: ", ")
            XCTAssertTrue(log.hasPrefix("connection CONNECT, receiver LAUNCH, connection CONNECT, media LOAD"), log)
            XCTAssertEqual(tv.tracks.map(\.0), ["text/vtt; charset=utf-8"], "WebVTT")
            XCTAssertEqual(tv.tracks.map(\.1), [true], "with CORS")
            XCTAssertEqual(tv.activeTracks, [1])
        }
        XCTAssertNoThrow(try s.ask(.cmd(["do": "pause"])).get())
        XCTAssertEqual(states.until("paused")["by"] as? String, "sender")
        XCTAssertNoThrow(try s.ask(.cmd(["do": "seek", "pos": 1])).get())
        states.until("paused at 1") { $0["state"] as? String == "paused" && $0["pos"] as? Double == 1 }
        XCTAssertNoThrow(try s.ask(.cmd(["do": "sub", "id": NSNull()])).get())
        XCTAssertTrue(fake.with { $0.activeTracks.isEmpty })
        XCTAssertNoThrow(try s.ask(.cmd(["do": "rate", "rate": 2.0])).get())
        states.until("rate 2") { $0["rate"] as? Double == 2 }
        XCTAssertThrowsError(try s.ask(.cmd(["do": "audio", "id": "a1"])).get())
        XCTAssertEqual(try s.ask(.volume(["t": "volume", "level": 40])).get() as NSDictionary, ["level": 40, "muted": false])
        XCTAssertEqual(try s.ask(.volume(["t": "volume", "step": 1])).get() as NSDictionary, ["level": 45, "muted": false])
        XCTAssertEqual(fake.with { $0.volume }, 0.45, accuracy: 1e-9)
        XCTAssertNoThrow(try s.ask(.cmd(["do": "play"])).get())
        states.until("playing")
        // Paused with the TV's remote: told as the receiver's doing.
        Thread.sleep(forTimeInterval: 3)
        fake.pressPause()
        XCTAssertEqual(states.until("paused")["by"] as? String, "receiver")
        XCTAssertNoThrow(try s.ask(.cmd(["do": "seek", "pos": 28])).get())
        XCTAssertNoThrow(try s.ask(.cmd(["do": "play"])).get())
        states.until("ended")
        XCTAssertEqual(states.until("stopped")["reason"] as? String, "ended")
    }

    func testAChromecastSessionEndsWhenStoppedOrTakenOver() throws {
        let fake = try FakeCast(dur: 600)
        let d = castDevice(fake.port)
        let srv = try files()
        let item = Array(videoAndSubs().prefix(1))
        var states = States()
        let s = try play(d, try media(item, srv), onState: states.sink)
        states.until("playing")
        XCTAssertNoThrow(try s.ask(.stop).get())
        XCTAssertEqual(states.until("stopped")["reason"] as? String, "sender")
        XCTAssertNil(fake.with { $0.app }, "our app is closed on the TV")

        states = States()
        _ = try play(d, try media(item, srv), onState: states.sink)
        states.until("playing")
        fake.takeOver()
        XCTAssertEqual(states.until("stopped")["reason"] as? String, "replaced")
    }

    func testAChromecastRefusesWhatItCannotFetch() throws {
        let fake = try FakeCast(dur: 60)
        var m = Media()
        m.url = "http://127.0.0.1:9/nothing.mp4"
        m.mime = "video/mp4"
        XCTAssertThrowsError(try play(castDevice(fake.port), m)) { e in
            XCTAssertTrue(e.localizedDescription.contains("LOAD_FAILED"), e.localizedDescription)
        }
    }

    /// A Cast receiver drops a sender that stays silent; a session that lasts longer than its timeout must keep
    /// sending PINGs and answering the TV's.
    func testAChromecastSessionKeepsItsHeartbeat() throws {
        let fake = try FakeCast(dur: 600)
        let srv = try files()
        let states = States()
        let s = try play(castDevice(fake.port), try media(Array(videoAndSubs().prefix(1)), srv), onState: states.sink)
        states.until("playing")
        Thread.sleep(forTimeInterval: 11)
        XCTAssertGreaterThanOrEqual(fake.with { $0.pingsFromSender }, 2)
        XCTAssertEqual(s.state["state"] as? String, "playing")
        XCTAssertNoThrow(try s.ask(.cmd(["do": "state"])).get())
    }

    // MARK: - through the local API

    /// The whole way luchctl and the menu go: both fakes at one address are one row with two ways; `cast` plays on the
    /// best (Cast), the hidden choice on the other (DLNA); media_state, media_cmd, volume and stop_cast work on both,
    /// and what only Luch does says so.
    func testTVsPlayThroughTheLocalAPI() throws {
        let dlnaTV = FakeRenderer(dur: 600)
        let castTV = try FakeCast(dur: 600)
        let tvs = TVs()
        tvs.saw(try dlnaDevice(dlnaTV))
        tvs.saw(castDevice(castTV.port))
        let srv = try files()
        let states = States()
        tvs.onState = { _, v in states.add(v) }
        let groups = tvs.groups(luch: [])
        XCTAssertEqual(groups.count, 1, "one TV, one row: \(groups)")
        let id = groups[0][0].id
        XCTAssertTrue(id.hasPrefix("cast:"), "Cast ranks above DLNA")
        XCTAssertEqual(groups[0].map(\.kind), [.cast, .dlna])
        func run(_ r: [String: Any]) -> [String: Any] {
            tvs.handle(r, row: id, luch: [], files: { srv }) ?? ["error": "not a TV's command"]
        }
        XCTAssertEqual(run(["cmd": "receiver_modes"])["modes"] as? [String], ["media", "photo"])
        let item = [videoAndSubs()[0]]
        XCTAssertEqual(run(["cmd": "cast", "items": item, "options": ["title": "Holiday"]])["kind"] as? String, "media")
        states.until("playing")
        XCTAssertTrue(castTV.with { $0.log.contains("media LOAD") })
        XCTAssertEqual((run(["cmd": "media_state"])["state"] as? [String: Any])?["state"] as? String, "playing")
        XCTAssertNil(run(["cmd": "media_cmd", "media": ["do": "pause"]])["error"])
        states.until("paused")
        XCTAssertEqual((run(["cmd": "volume", "what": "40"])["volume"] as? [String: Any])?["level"] as? Int, 40)
        XCTAssertNotNil(run(["cmd": "page", "what": "next"])["error"])
        XCTAssertNil(run(["cmd": "stop_cast"])["error"])
        XCTAssertEqual(states.until("stopped")["reason"] as? String, "sender")

        // The hidden choice: the same row over DLNA.
        XCTAssertEqual(run(["cmd": "cast", "items": item, "options": ["transport": "dlna"]])["kind"] as? String, "media")
        states.until("playing")
        XCTAssertTrue(dlnaTV.actions.contains("SetAVTransportURI"))
        XCTAssertNil(run(["cmd": "media_cmd", "media": ["do": "seek", "pos": 120]])["error"])
        XCTAssertNil(run(["cmd": "stop_cast"])["error"])
        XCTAssertEqual(states.until("stopped")["reason"] as? String, "sender")
        XCTAssertEqual(dlnaTV.with { $0.state }, "STOPPED")
        let e = run(["cmd": "cast", "items": item, "options": ["transport": "luch"]])["error"] as? String ?? ""
        XCTAssertTrue(e.contains("cannot be reached that way"), e)
        XCTAssertEqual((run(["cmd": "media_state"])["state"] as? [String: Any])?.count, 0)
        XCTAssertNotNil(run(["cmd": "volume", "what": "+"])["error"], "a TV's volume only while something plays")
    }
}
