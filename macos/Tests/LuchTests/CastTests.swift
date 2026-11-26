// SPDX-License-Identifier: GPL-3.0-or-later
import Foundation
import XCTest
@testable import LuchCast
@testable import LuchEngine

final class CastTests: XCTestCase {
    private func temp(_ name: String, _ data: Data) -> URL {
        let dir = FileManager.default.temporaryDirectory.appendingPathComponent("luch-cast-\(UUID().uuidString)")
        try? FileManager.default.createDirectory(at: dir, withIntermediateDirectories: true)
        let u = dir.appendingPathComponent(name)
        try? data.write(to: u)
        return u
    }

    private func server() throws -> FileServer {
        let s = FileServer()
        try s.start(port: 0, address: "127.0.0.1")
        return s
    }

    /// A plain GET with an optional Range: (status, headers, body).
    private func get(_ url: String, range: String? = nil) -> (Int, [String: String], Data) {
        var r = URLRequest(url: URL(string: url)!)
        if let range { r.setValue(range, forHTTPHeaderField: "Range") }
        r.cachePolicy = .reloadIgnoringLocalCacheData
        let done = DispatchSemaphore(value: 0)
        var out: (Int, [String: String], Data) = (0, [:], Data())
        URLSession(configuration: .ephemeral).dataTask(with: r) { data, response, _ in
            let h = response as? HTTPURLResponse
            var headers: [String: String] = [:]
            for (k, v) in h?.allHeaderFields ?? [:] { headers[String(describing: k).lowercased()] = String(describing: v) }
            out = (h?.statusCode ?? 0, headers, data ?? Data())
            done.signal()
        }.resume()
        _ = done.wait(timeout: .now() + 10)
        return out
    }

    func testRanges() {
        XCTAssertEqual(FileServer.parseRange("bytes=0-", size: 100)!.0, 0)
        XCTAssertEqual(FileServer.parseRange("bytes=10-19", size: 100)!.1, 19)
        XCTAssertEqual(FileServer.parseRange("bytes=90-200", size: 100)!.1, 99)
        XCTAssertEqual(FileServer.parseRange("bytes=-10", size: 100)!.0, 90)
        XCTAssertNil(FileServer.parseRange("bytes=100-", size: 100))
        XCTAssertNil(FileServer.parseRange("items=0-1", size: 100))
    }

    func testFilesAreServedWithRangeToTheirReceiverOnly() throws {
        let s = try server()
        let data = Data((0..<10_000).map { UInt8($0 % 256) })
        let file = temp("clip.mp4", data)
        let t = s.share(.file(file), client: "127.0.0.1", mime: "video/mp4")
        let url = s.url(host: "127.0.0.1", token: t, name: "clip.mp4")
        let whole = get(url)
        XCTAssertEqual(whole.0, 200)
        XCTAssertEqual(whole.1["content-type"], "video/mp4")
        XCTAssertEqual(whole.2, data)
        let part = get(url, range: "bytes=1000-1099")
        XCTAssertEqual(part.0, 206)
        XCTAssertEqual(part.1["content-range"], "bytes 1000-1099/10000")
        XCTAssertEqual(part.2, data[1000..<1100])
        XCTAssertEqual(get(url, range: "bytes=20000-").0, 416)
        XCTAssertEqual(get("http://127.0.0.1:\(s.port)/nope/clip.mp4").0, 404)
        let other = s.share(.file(file), client: "192.168.1.30", mime: "video/mp4")
        XCTAssertEqual(get(s.url(host: "127.0.0.1", token: other, name: "clip.mp4")).0, 403)
        s.forget(client: "127.0.0.1")
        XCTAssertEqual(get(url).0, 404)
    }

    func testRelayRewritesPlaylistsToPointBackHere() throws {
        let origin = try server()
        let dir = temp("index.m3u8", Data("#EXTM3U\n#EXT-X-KEY:METHOD=AES-128,URI=\"key.bin\"\n#EXTINF:4,\nseg0.ts\n".utf8))
            .deletingLastPathComponent()
        try Data("segment".utf8).write(to: dir.appendingPathComponent("seg0.ts"))
        let pt = origin.share(.file(dir.appendingPathComponent("index.m3u8")), client: "127.0.0.1", mime: "application/vnd.apple.mpegurl")
        // Each file of the origin has a token of its own, so its playlist names the segment by an absolute link.
        let seg = origin.share(.file(dir.appendingPathComponent("seg0.ts")), client: "127.0.0.1", mime: "video/mp2t")
        let text = "#EXTM3U\n#EXTINF:4,\n" + origin.url(host: "127.0.0.1", token: seg, name: "seg0.ts") + "\n"
        try Data(text.utf8).write(to: dir.appendingPathComponent("index.m3u8"))

        let relay = try server()
        let rt = relay.share(.relay(url: origin.url(host: "127.0.0.1", token: pt, name: "index.m3u8"), headers: ["Referer": "https://x.example/"]),
                             client: "127.0.0.1", mime: "")
        let (code, _, body) = get(relay.url(host: "127.0.0.1", token: rt, name: "index.m3u8"))
        XCTAssertEqual(code, 200)
        let rewritten = String(decoding: body, as: UTF8.self)
        let links = rewritten.split(separator: "\n").filter { !$0.hasPrefix("#") }
        XCTAssertEqual(links.count, 1, rewritten)
        XCTAssertTrue(links[0].hasPrefix("http://127.0.0.1:\(relay.port)/"), rewritten)
        XCTAssertEqual(String(decoding: get(String(links[0])).2, as: UTF8.self), "segment")
    }

    func testPlaylistLinksResolve() {
        let b = "https://cdn.example/a/b/index.m3u8?t=1"
        XCTAssertEqual(Playlist.resolve(base: b, "seg.ts"), "https://cdn.example/a/b/seg.ts")
        XCTAssertEqual(Playlist.resolve(base: b, "../c/seg.ts"), "https://cdn.example/a/c/seg.ts")
        XCTAssertEqual(Playlist.resolve(base: b, "/root.ts"), "https://cdn.example/root.ts")
        XCTAssertEqual(Playlist.resolve(base: b, "//other.example/x.ts"), "https://other.example/x.ts")
        let out = Playlist.rewrite("#EXT-X-KEY:URI=\"k.bin\"\nseg.ts\n", base: b) { "R(\($0))" }
        XCTAssertTrue(out.contains("URI=\"R(https://cdn.example/a/b/k.bin)\""), out)
        XCTAssertTrue(out.contains("R(https://cdn.example/a/b/seg.ts)"), out)
    }

    func testYoutubeLinks() {
        let id = "dQw4w9WgXcQ"
        for u in ["https://www.youtube.com/watch?v=\(id)", "https://youtu.be/\(id)", "https://www.youtube.com/shorts/\(id)",
                  "https://www.youtube-nocookie.com/embed/\(id)?rel=0", "https://music.youtube.com/watch?v=\(id)&list=x"] {
            XCTAssertEqual(Cast.youtube(u)?.id, id, u)
        }
        XCTAssertEqual(Cast.youtube("https://youtu.be/\(id)?t=1m30s")?.start, 90)
        XCTAssertNil(Cast.youtube("https://www.youtube.com/feed/subscriptions"))
        XCTAssertNil(Cast.youtube("https://notyoutube.com/watch?v=\(id)"))
    }

    func testMessagesForFilesLinksPhotosDocumentsAndYoutube() throws {
        let s = try server()
        let video = temp("Holiday 2026.mp4", Data("x".utf8))
        let sub = video.deletingLastPathComponent().appendingPathComponent("Holiday 2026.ru.vtt")
        try Data("WEBVTT\n".utf8).write(to: sub)
        var o = CastOptions()
        o.start = 12
        let m = try Cast.build([video.path, sub.path], o: o, server: s, client: "192.168.1.30", me: "192.168.1.20")
        XCTAssertEqual(m["t"] as? String, "media")
        XCTAssertEqual(m["via"] as? String, "sender")
        XCTAssertEqual(m["title"] as? String, "Holiday 2026")
        let src = (m["src"] as? [[String: Any]])?.first
        XCTAssertTrue((src?["url"] as? String ?? "").hasPrefix("http://192.168.1.20:\(s.port)/"))
        XCTAssertTrue((src?["url"] as? String ?? "").hasSuffix("/Holiday%202026.mp4"))
        XCTAssertEqual((m["subs"] as? [[String: Any]])?.first?["lang"] as? String, "ru")

        let o2 = try CastOptions(json: ["headers": ["cookie": "a=1", "Host": "evil"]])
        let link = try Cast.build(["https://cdn.example/v/index.m3u8"], o: o2, server: s, client: "192.168.1.30", me: "192.168.1.20")
        XCTAssertEqual(link["via"] as? String, "direct")
        XCTAssertEqual(link["headers"] as? [String: String], ["Cookie": "a=1"])

        let yt = try Cast.build(["https://youtu.be/dQw4w9WgXcQ?t=42"], o: CastOptions(), server: s, client: "192.168.1.30", me: "192.168.1.20")
        XCTAssertEqual(yt["t"] as? String, "open_app")
        XCTAssertEqual(yt["start"] as? Double, 42)

        let pdf = temp("Report.pdf", Data("%PDF-1.4".utf8))
        let doc = try Cast.build([pdf.path], o: CastOptions(), server: s, client: "192.168.1.30", me: "192.168.1.20")
        XCTAssertEqual(doc["t"] as? String, "doc")

        let a = temp("a.jpg", Data("j".utf8))
        let photos = try Cast.build([a.path], o: CastOptions(), server: s, client: "192.168.1.30", me: "192.168.1.20")
        XCTAssertEqual(photos["t"] as? String, "photo")

        XCTAssertThrowsError(try Cast.build(["/nonexistent/x.mp4"], o: CastOptions(), server: s, client: "1.2.3.4", me: "1.2.3.5"))
        XCTAssertThrowsError(try CastOptions(json: ["via": "pigeon"]))
    }

    func testTextBecomesPdfOnThisMac() throws {
        let txt = temp("notes.txt", Data("Luch\nпривет\n".utf8))
        let pdf = try Cast.toPDF(txt, outDir: txt.deletingLastPathComponent().appendingPathComponent("pdf"))
        XCTAssertTrue(try Data(contentsOf: pdf).starts(with: Data("%PDF".utf8)))
    }

    func testMediaKeyCodes() {
        XCTAssertEqual(MediaKeys.code("play_pause"), 16)
        XCTAssertEqual(MediaKeys.code("next"), 17)
        XCTAssertEqual(MediaKeys.code("previous"), 18)
        XCTAssertNil(MediaKeys.code("dance"))
    }
}
