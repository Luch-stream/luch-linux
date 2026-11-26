// What to send a receiver for a file or a link (PROTOCOL.md 4a, 4b, 4f, 4g): photos, a video or a sound it plays
// itself, a PDF, or a YouTube video for the TV's own app. The rules are luchd's (luch-linux luchd/src/cast.rs).
// SPDX-License-Identifier: GPL-3.0-or-later
import Foundation

public enum CastKind: Equatable {
    case image, media, subtitle, document, unknown
}

public struct CastOptions {
    public var title: String?
    public var start: Double = 0
    public var subs: [String] = []
    /// "direct" (the receiver fetches a link itself) or "sender" (relayed by this Mac); files always go via the sender.
    public var via: String?
    public var headers: [String: String] = [:]
    public var paused = false
    public var rate: Double?
    public var mime: String?
    public var poster: String?
    /// The same video in several qualities, best first ([url, mime, w, h]); the receiver plays what its screen shows.
    public var sources: [[String: Any]] = []

    public static let passedHeaders = ["Referer", "User-Agent", "Origin", "Cookie", "Authorization"]

    public init() {}

    public init(json: [String: Any]) throws {
        title = json["title"] as? String
        start = max(0, (json["start"] as? NSNumber)?.doubleValue ?? 0)
        via = json["via"] as? String
        if let v = via, v != "direct" && v != "sender" {
            throw CastError.bad("via must be direct or sender, not \(v)")
        }
        paused = (json["paused"] as? Bool) ?? false
        rate = (json["rate"] as? NSNumber)?.doubleValue
        mime = json["mime"] as? String
        poster = json["poster"] as? String
        subs = json["subs"] as? [String] ?? []
        sources = (json["sources"] as? [[String: Any]] ?? [])
            .filter { ($0["url"] as? String).map(Cast.isURL) ?? false }
            .prefix(12).map { $0 }
        for (k, v) in json["headers"] as? [String: Any] ?? [:] {
            guard let name = CastOptions.passedHeaders.first(where: { $0.caseInsensitiveCompare(k) == .orderedSame }),
                  let value = v as? String, !value.contains("\r"), !value.contains("\n") else { continue }
            headers[name] = value
        }
    }
}

public enum CastError: Error, LocalizedError, Equatable {
    case bad(String)
    public var errorDescription: String? {
        switch self { case .bad(let s): return s }
    }
}

public enum Cast {
    static func ext(_ s: String) -> String {
        let path = s.split(whereSeparator: { $0 == "?" || $0 == "#" }).first.map(String.init) ?? s
        let name = path.split(separator: "/").last.map(String.init) ?? path
        guard let dot = name.lastIndex(of: ".") else { return "" }
        return String(name[name.index(after: dot)...]).lowercased()
    }

    static func isURL(_ s: String) -> Bool { s.hasPrefix("http://") || s.hasPrefix("https://") }

    public static func mime(_ s: String) -> String {
        switch ext(s) {
        case "mp4", "m4v", "mov": return "video/mp4"
        case "mkv": return "video/x-matroska"
        case "webm": return "video/webm"
        case "ts", "m2ts": return "video/mp2t"
        case "m3u8": return "application/vnd.apple.mpegurl"
        case "mpd": return "application/dash+xml"
        case "mp3": return "audio/mpeg"
        case "m4a", "m4b": return "audio/mp4"
        case "aac": return "audio/aac"
        case "ogg", "oga", "opus": return "audio/ogg"
        case "flac": return "audio/flac"
        case "wav": return "audio/wav"
        case "jpg", "jpeg": return "image/jpeg"
        case "png": return "image/png"
        case "webp": return "image/webp"
        case "gif": return "image/gif"
        case "heic", "heif": return "image/heif"
        case "vtt": return "text/vtt"
        case "srt": return "application/x-subrip"
        case "pdf": return "application/pdf"
        default: return ""
        }
    }

    /// Text macOS itself turns into PDF (cupsfilter); office files need LibreOffice, as on Linux.
    static let textLike: Set<String> = ["txt", "md", "log", "csv", "json", "yaml", "yml", "toml", "xml", "py", "rs",
                                        "js", "ts", "c", "h", "cpp", "swift", "go", "sh", "java", "kt", "rb", "sql"]
    static let office: Set<String> = ["doc", "docx", "odt", "rtf", "xls", "xlsx", "ods", "ppt", "pptx", "odp"]

    public static func kind(_ s: String) -> CastKind {
        let e = ext(s)
        if e == "pdf" || (!isURL(s) && (textLike.contains(e) || office.contains(e))) { return .document }
        let m = mime(s)
        if m.hasPrefix("image/") { return .image }
        if m.hasPrefix("text/") || m == "application/x-subrip" { return .subtitle }
        if m.hasPrefix("video/") || m.hasPrefix("audio/") || m.hasPrefix("application/") { return .media }
        return .unknown
    }

    /// The video id and start (s) of a YouTube link: watch, youtu.be, shorts, embed, live; `t=` as 90, 90s, 1m30s.
    public static func youtube(_ link: String) -> (id: String, start: Double)? {
        guard let c = URLComponents(string: link), var host = c.host?.lowercased() else { return nil }
        for p in ["www.", "m.", "music."] where host.hasPrefix(p) { host.removeFirst(p.count) }
        let parts = c.path.split(separator: "/").map(String.init)
        let q = { (k: String) in c.queryItems?.first(where: { $0.name == k })?.value }
        var id: String?
        switch host {
        case "youtu.be": id = parts.first
        case "youtube.com", "youtube-nocookie.com":
            if parts.first == "watch" { id = q("v") }
            else if let f = parts.first, ["shorts", "embed", "live", "v"].contains(f), parts.count > 1 { id = parts[1] }
        default: return nil
        }
        guard let id, (6...20).contains(id.count),
              id.allSatisfy({ $0.isASCII && ($0.isLetter || $0.isNumber || $0 == "-" || $0 == "_") }) else { return nil }
        return (id, (q("t") ?? q("start")).map(seconds) ?? 0)
    }

    static func seconds(_ t: String) -> Double {
        if let n = Double(t) { return n }
        var total = 0.0, num = ""
        for ch in t {
            if ch.isNumber { num.append(ch); continue }
            let n = Double(num) ?? 0
            num = ""
            total += n * (ch == "h" ? 3600 : ch == "m" ? 60 : 1)
        }
        return total + (Double(num) ?? 0)
    }

    /// A document as PDF: a PDF as it is; text through cupsfilter; office files through LibreOffice when installed.
    public static func toPDF(_ path: URL, outDir: URL) throws -> URL {
        let e = ext(path.path)
        if e == "pdf" { return path }
        try FileManager.default.createDirectory(at: outDir, withIntermediateDirectories: true)
        let out = outDir.appendingPathComponent(path.deletingPathExtension().lastPathComponent + ".pdf")
        let p = Process()
        if textLike.contains(e) {
            p.executableURL = URL(fileURLWithPath: "/usr/sbin/cupsfilter")
            p.arguments = [path.path]
            FileManager.default.createFile(atPath: out.path, contents: nil)
            p.standardOutput = try FileHandle(forWritingTo: out)
            p.standardError = FileHandle.nullDevice
        } else {
            let soffice = ["/Applications/LibreOffice.app/Contents/MacOS/soffice", "/opt/homebrew/bin/soffice"]
                .first { FileManager.default.isExecutableFile(atPath: $0) }
            guard let soffice else { throw CastError.bad("install LibreOffice to show this kind of document (PDF works without it)") }
            p.executableURL = URL(fileURLWithPath: soffice)
            p.arguments = ["--headless", "--convert-to", "pdf", "--outdir", outDir.path, path.path]
        }
        try p.run()
        p.waitUntilExit()
        let ok = (try? Data(contentsOf: out))?.starts(with: Data("%PDF".utf8)) ?? false
        guard p.terminationStatus == 0, ok else { throw CastError.bad("could not turn \(path.lastPathComponent) into PDF") }
        return out
    }

    static func fileURL(_ s: String) throws -> URL {
        let raw = s.hasPrefix("file://") ? (URL(string: s)?.path ?? s) : s
        let u = URL(fileURLWithPath: (raw as NSString).expandingTildeInPath).standardizedFileURL
        var dir: ObjCBool = false
        guard FileManager.default.fileExists(atPath: u.path, isDirectory: &dir), !dir.boolValue else {
            throw CastError.bad("\(s): not a file")
        }
        return u
    }

    static func name(_ s: String) -> String {
        let path = s.split(whereSeparator: { $0 == "?" || $0 == "#" }).first.map(String.init) ?? s
        return path.split(separator: "/").last.map(String.init) ?? path
    }

    static func subLang(_ name: String) -> String {
        let parts = name.split(separator: ".")
        guard parts.count >= 3 else { return "" }
        let l = parts[parts.count - 2]
        return (2...3).contains(l.count) && l.allSatisfy(\.isLetter) ? l.lowercased() : ""
    }

    /// Shares one file or link; returns (url for the receiver, via).
    static func shareOne(_ s: String, mime: String, relay: Bool, o: CastOptions, server: FileServer,
                         client: String, me: String) throws -> (String, String) {
        if isURL(s) {
            if !relay { return (s, "direct") }
            let t = server.share(.relay(url: s, headers: o.headers), client: client, mime: mime)
            return (server.url(host: me, token: t, name: name(s)), "sender")
        }
        let u = try fileURL(s)
        let t = server.share(.file(u), client: client, mime: mime.isEmpty ? "application/octet-stream" : mime)
        return (server.url(host: me, token: t, name: u.lastPathComponent), "sender")
    }

    /// The message for `items` (paths or http(s) links); documents must be PDF by now (`toPDF`).
    public static func build(_ items: [String], o: CastOptions, server: FileServer, client: String, me: String) throws -> [String: Any] {
        guard !items.isEmpty else { throw CastError.bad("nothing to cast") }
        if items.count == 1, let (id, t) = youtube(items[0]) {
            return ["t": "open_app", "app": "youtube", "id": id, "start": o.start > 0 ? o.start : t]
        }
        if let d = items.first(where: { kind($0) == .document }) {
            guard !isURL(d) else { throw CastError.bad("a document has to be a file on this Mac") }
            let u = try fileURL(d)
            guard ext(u.path) == "pdf" else { throw CastError.bad("\(u.lastPathComponent): not a PDF yet") }
            let t = server.share(.file(u), client: client, mime: "application/pdf")
            return ["t": "doc", "url": server.url(host: me, token: t, name: u.lastPathComponent), "mime": "application/pdf",
                    "name": o.title ?? u.lastPathComponent, "page": Int(o.start)]
        }
        if items.allSatisfy({ kind($0) == .image }) {
            var list: [[String: Any]] = []
            for i in items.prefix(200) {
                let (u, _) = try shareOne(i, mime: mime(i), relay: true, o: o, server: server, client: client, me: me)
                list.append(["url": u, "mime": mime(i), "name": name(i)])
            }
            return ["t": "photo", "index": 0, "items": list]
        }
        guard let first = items.first(where: { kind($0) == .media || (isURL($0) && kind($0) == .unknown) }) else {
            throw CastError.bad("no video or sound among the items")
        }
        let relay = o.via == "sender"
        let m = o.mime ?? mime(first)
        let (url, via) = try shareOne(first, mime: m, relay: relay, o: o, server: server, client: client, me: me)
        var src: [String: Any] = ["url": url]
        if !m.isEmpty { src["mime"] = m }
        var list: [[String: Any]] = []
        for s in o.sources {
            // Every quality travels the way the first one does.
            let sm = s["mime"] as? String ?? m
            let (u, _) = try shareOne(s["url"] as? String ?? "", mime: sm, relay: via == "sender", o: o, server: server,
                                      client: client, me: me)
            var e: [String: Any] = ["url": u]
            if !sm.isEmpty { e["mime"] = sm }
            for k in ["w", "h", "codec"] { if let v = s[k] { e[k] = v } }
            list.append(e)
        }
        if list.isEmpty { list = [src] }
        var title = o.title ?? name(first)
        if o.title == nil, let dot = title.lastIndex(of: ".") { title = String(title[..<dot]) }
        var msg: [String: Any] = ["t": "media", "via": via, "src": list, "title": title]
        if o.start > 0 { msg["start"] = o.start }
        if o.paused { msg["paused"] = true }
        if let r = o.rate { msg["rate"] = r }
        if via == "direct" && !o.headers.isEmpty { msg["headers"] = o.headers }
        var subs: [[String: Any]] = []
        for (n, s) in (o.subs + items.filter { kind($0) == .subtitle }).enumerated() {
            let (u, _) = try shareOne(s, mime: mime(s), relay: relay || via == "sender", o: o, server: server, client: client, me: me)
            let lang = subLang(name(s))
            subs.append(["id": "s\(n)", "url": u, "mime": mime(s), "lang": lang, "label": lang.isEmpty ? name(s) : lang])
        }
        if !subs.isEmpty { msg["subs"] = subs }
        return msg
    }
}
