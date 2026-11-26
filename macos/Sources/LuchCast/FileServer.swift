// What a receiver fetches when something is cast (luch-android PROTOCOL.md 4a, 4b, 4g): local files with Range, and
// links relayed through this Mac (fetched with the page's headers, passed on unchanged; HLS playlists rewritten to
// point back here). Each thing has a random token in its path and answers only the receiver it was shared with.
// The same design as luch-linux's luchd/src/cast.rs.
// SPDX-License-Identifier: GPL-3.0-or-later
import Darwin
import Foundation

public enum CastItem: Equatable {
    case file(URL)
    case relay(url: String, headers: [String: String])
}

public final class FileServer {
    /// The port receivers fetch from, fixed so a firewall can be opened for it (as on Linux).
    public static let defaultPort: UInt16 = 47810

    private struct Entry {
        let item: CastItem
        let client: String
        let mime: String
    }

    public private(set) var port: UInt16 = 0
    private var listenFD: Int32 = -1
    private var shared: [String: Entry] = [:]
    private let lock = NSLock()
    private let session: URLSession

    public init() {
        let config = URLSessionConfiguration.ephemeral
        config.timeoutIntervalForRequest = 15
        session = URLSession(configuration: config)
    }

    /// Listens on `port` (any free one when it is taken) on `address` ("0.0.0.0" for every interface).
    public func start(port: UInt16 = FileServer.defaultPort, address: String = "0.0.0.0") throws {
        func open(_ p: UInt16) -> Int32 {
            let fd = socket(AF_INET, SOCK_STREAM, 0)
            guard fd >= 0 else { return -1 }
            var yes: Int32 = 1
            setsockopt(fd, SOL_SOCKET, SO_REUSEADDR, &yes, socklen_t(MemoryLayout<Int32>.size))
            var a = sockaddr_in()
            a.sin_len = UInt8(MemoryLayout<sockaddr_in>.size)
            a.sin_family = sa_family_t(AF_INET)
            a.sin_port = p.bigEndian
            inet_pton(AF_INET, address, &a.sin_addr)
            let ok = withUnsafePointer(to: &a) {
                $0.withMemoryRebound(to: sockaddr.self, capacity: 1) { bind(fd, $0, socklen_t(MemoryLayout<sockaddr_in>.size)) }
            } == 0 && listen(fd, 16) == 0
            if !ok { close(fd); return -1 }
            return fd
        }
        var fd = open(port)
        if fd < 0 { fd = open(0) }
        guard fd >= 0 else { throw NSError(domain: "luch.cast", code: 1, userInfo: [NSLocalizedDescriptionKey: "cannot listen"]) }
        var a = sockaddr_in()
        var len = socklen_t(MemoryLayout<sockaddr_in>.size)
        withUnsafeMutablePointer(to: &a) { $0.withMemoryRebound(to: sockaddr.self, capacity: 1) { _ = getsockname(fd, $0, &len) } }
        self.port = UInt16(bigEndian: a.sin_port)
        listenFD = fd
        Thread.detachNewThread { [weak self] in self?.acceptLoop() }
    }

    public func stop() {
        if listenFD >= 0 { close(listenFD) }
        listenFD = -1
    }

    /// Makes `item` fetchable by `client` (its IPv4 address); returns its token.
    public func share(_ item: CastItem, client: String, mime: String) -> String {
        let token = UUID().uuidString.replacingOccurrences(of: "-", with: "").lowercased()
        lock.lock()
        shared[token] = Entry(item: item, client: client, mime: mime)
        lock.unlock()
        return token
    }

    public func url(host: String, token: String, name: String) -> String {
        let allowed = CharacterSet.alphanumerics.union(CharacterSet(charactersIn: "-._~"))
        let encoded = name.addingPercentEncoding(withAllowedCharacters: allowed) ?? "x"
        return "http://\(host):\(port)/\(token)/\(encoded)"
    }

    /// Nothing shared with `client` is served any more (its session ended).
    public func forget(client: String) {
        lock.lock()
        shared = shared.filter { $0.value.client != client }
        lock.unlock()
    }

    public var sharedCount: Int {
        lock.lock()
        defer { lock.unlock() }
        return shared.count
    }

    // MARK: - serving

    private func acceptLoop() {
        while listenFD >= 0 {
            var a = sockaddr_in()
            var len = socklen_t(MemoryLayout<sockaddr_in>.size)
            let fd = withUnsafeMutablePointer(to: &a) {
                $0.withMemoryRebound(to: sockaddr.self, capacity: 1) { accept(listenFD, $0, &len) }
            }
            if fd < 0 { if listenFD < 0 { return }; continue }
            var yes: Int32 = 1
            setsockopt(fd, SOL_SOCKET, SO_NOSIGPIPE, &yes, socklen_t(MemoryLayout<Int32>.size))
            var buf = [CChar](repeating: 0, count: Int(INET_ADDRSTRLEN))
            inet_ntop(AF_INET, &a.sin_addr, &buf, socklen_t(INET_ADDRSTRLEN))
            let peer = String(cString: buf)
            Thread.detachNewThread { [weak self] in
                self?.serve(fd: fd, peer: peer)
                close(fd)
            }
        }
    }

    struct Request {
        var path = ""
        var head = false
        var range: String?
        var close = false
    }

    /// Reads one request head; nil at the end of the connection.
    static func readRequest(_ fd: Int32, carry: inout Data) -> Request? {
        while carry.range(of: Data("\r\n\r\n".utf8)) == nil {
            if carry.count > 16 * 1024 { return nil }
            var chunk = [UInt8](repeating: 0, count: 4096)
            let n = recv(fd, &chunk, chunk.count, 0)
            if n <= 0 { return nil }
            carry.append(contentsOf: chunk[0..<n])
        }
        let end = carry.range(of: Data("\r\n\r\n".utf8))!
        let head = String(decoding: carry[carry.startIndex..<end.lowerBound], as: UTF8.self)
        carry.removeSubrange(carry.startIndex..<end.upperBound)
        var lines = head.components(separatedBy: "\r\n")
        let first = lines.removeFirst().split(separator: " ")
        var r = Request()
        let method = first.first.map(String.init) ?? ""
        r.path = first.count > 1 ? String(first[1]) : "/"
        r.head = method == "HEAD"
        if first.count > 2 && first[2] == "HTTP/1.0" { r.close = true }
        if method != "GET" && method != "HEAD" { r.path = "" }
        for l in lines {
            guard let colon = l.firstIndex(of: ":") else { continue }
            let k = l[..<colon].trimmingCharacters(in: .whitespaces).lowercased()
            let v = l[l.index(after: colon)...].trimmingCharacters(in: .whitespaces)
            if k == "range" { r.range = v }
            if k == "connection" && v.lowercased() == "close" { r.close = true }
        }
        return r
    }

    private func serve(fd: Int32, peer: String) {
        var carry = Data()
        while let req = FileServer.readRequest(fd, carry: &carry) {
            let token = req.path.split(separator: "/").first.map(String.init) ?? ""
            lock.lock()
            let entry = shared[token]
            lock.unlock()
            guard let entry else {
                if !FileServer.respond(fd, 404) || req.close { return }
                continue
            }
            guard entry.client == peer else {
                if !FileServer.respond(fd, 403) || req.close { return }
                continue
            }
            let keep: Bool
            switch entry.item {
            case .file(let url): keep = FileServer.sendFile(fd, url: url, mime: entry.mime, req: req)
            case .relay(let url, let headers): keep = relay(fd, url: url, headers: headers, req: req, client: entry.client)
            }
            if !keep || req.close { return }
        }
    }

    static func reason(_ code: Int) -> String {
        switch code {
        case 200: return "OK"
        case 206: return "Partial Content"
        case 403: return "Forbidden"
        case 404: return "Not Found"
        case 416: return "Range Not Satisfiable"
        case 502: return "Bad Gateway"
        default: return "Status"
        }
    }

    @discardableResult
    static func write(_ fd: Int32, _ data: Data) -> Bool {
        data.withUnsafeBytes { (p: UnsafeRawBufferPointer) -> Bool in
            var off = 0
            while off < p.count {
                let n = Darwin.send(fd, p.baseAddress!.advanced(by: off), p.count - off, 0)
                if n <= 0 { if errno == EINTR { continue }; return false }
                off += n
            }
            return true
        }
    }

    @discardableResult
    static func respond(_ fd: Int32, _ code: Int, headers: [(String, String)] = [], body: Data = Data()) -> Bool {
        var head = "HTTP/1.1 \(code) \(reason(code))\r\n"
        for (k, v) in headers { head += "\(k): \(v)\r\n" }
        head += "Content-Length: \(body.count)\r\n\r\n"
        return write(fd, Data(head.utf8)) && write(fd, body)
    }

    /// `bytes=a-b`, `bytes=a-` or `bytes=-n` of `size` bytes: the first and last byte, or nil when unsatisfiable.
    public static func parseRange(_ r: String, size: UInt64) -> (UInt64, UInt64)? {
        guard size > 0, r.hasPrefix("bytes=") else { return nil }
        let spec = r.dropFirst(6).split(separator: ",").first.map(String.init) ?? ""
        let parts = spec.split(separator: "-", omittingEmptySubsequences: false).map { $0.trimmingCharacters(in: .whitespaces) }
        guard parts.count == 2 else { return nil }
        let start: UInt64, end: UInt64
        if parts[0].isEmpty {
            guard let n = UInt64(parts[1]) else { return nil }
            start = size > n ? size - n : 0
            end = size - 1
        } else {
            guard let s = UInt64(parts[0]) else { return nil }
            start = s
            if parts[1].isEmpty { end = size - 1 } else { guard let e = UInt64(parts[1]) else { return nil }; end = min(e, size - 1) }
        }
        return start <= end && start < size ? (start, end) : nil
    }

    static func sendFile(_ fd: Int32, url: URL, mime: String, req: Request) -> Bool {
        guard let h = try? FileHandle(forReadingFrom: url),
              let size = (try? FileManager.default.attributesOfItem(atPath: url.path)[.size] as? NSNumber)?.uint64Value else {
            return respond(fd, 404)
        }
        defer { try? h.close() }
        var code = 200
        var (start, end): (UInt64, UInt64) = (0, size == 0 ? 0 : size - 1)
        if let r = req.range {
            guard let range = parseRange(r, size: size) else {
                return respond(fd, 416, headers: [("Content-Range", "bytes */\(size)")])
            }
            (start, end) = range
            code = 206
        }
        let length = size == 0 ? 0 : end - start + 1
        var head = "HTTP/1.1 \(code) \(reason(code))\r\nContent-Type: \(mime)\r\nAccept-Ranges: bytes\r\nContent-Length: \(length)\r\n"
        if code == 206 { head += "Content-Range: bytes \(start)-\(end)/\(size)\r\n" }
        head += "\r\n"
        guard write(fd, Data(head.utf8)) else { return false }
        if req.head || length == 0 { return true }
        do { try h.seek(toOffset: start) } catch { return false }
        var left = length
        while left > 0 {
            let chunk = h.readData(ofLength: Int(min(left, 256 * 1024)))
            if chunk.isEmpty { return false }
            guard write(fd, chunk) else { return false }
            left -= UInt64(chunk.count)
        }
        return true
    }

    // MARK: - relay

    private func localAddress(_ fd: Int32) -> String {
        var a = sockaddr_in()
        var len = socklen_t(MemoryLayout<sockaddr_in>.size)
        withUnsafeMutablePointer(to: &a) { $0.withMemoryRebound(to: sockaddr.self, capacity: 1) { _ = getsockname(fd, $0, &len) } }
        var buf = [CChar](repeating: 0, count: Int(INET_ADDRSTRLEN))
        inet_ntop(AF_INET, &a.sin_addr, &buf, socklen_t(INET_ADDRSTRLEN))
        return String(cString: buf)
    }

    private func relay(_ fd: Int32, url: String, headers: [String: String], req: Request, client: String) -> Bool {
        guard let u = URL(string: url) else { return FileServer.respond(fd, 502) }
        var r = URLRequest(url: u)
        for (k, v) in headers { r.setValue(v, forHTTPHeaderField: k) }
        if let range = req.range { r.setValue(range, forHTTPHeaderField: "Range") }
        let pipe = RelayPipe(fd: fd, head: req.head)
        let task = session.dataTask(with: r)
        task.delegate = pipe
        task.resume()
        pipe.done.wait()
        if pipe.failed && !pipe.wroteHead { return FileServer.respond(fd, 502) }
        if let playlist = pipe.playlist {
            let me = localAddress(fd)
            let base = pipe.finalURL ?? url
            let text = String(decoding: playlist, as: UTF8.self)
            let out = Playlist.rewrite(text, base: base) { abs in
                let t = self.share(.relay(url: abs, headers: headers), client: client, mime: "")
                return self.url(host: me, token: t, name: Playlist.lastSegment(abs))
            }
            return FileServer.respond(fd, 200, headers: [("Content-Type", "application/vnd.apple.mpegurl")],
                                      body: req.head ? Data() : Data(out.utf8))
        }
        return pipe.keepAlive && !pipe.failed
    }
}

/// Streams a URLSession response to the receiver's socket as it comes; a playlist is kept whole instead.
final class RelayPipe: NSObject, URLSessionDataDelegate {
    let fd: Int32
    let head: Bool
    let done = DispatchSemaphore(value: 0)
    var wroteHead = false
    var failed = false
    var keepAlive = true
    var playlist: Data?
    var finalURL: String?

    init(fd: Int32, head: Bool) {
        self.fd = fd
        self.head = head
    }

    func urlSession(_ session: URLSession, dataTask: URLSessionDataTask, didReceive response: URLResponse,
                    completionHandler: @escaping (URLSession.ResponseDisposition) -> Void) {
        let http = response as? HTTPURLResponse
        let status = http?.statusCode ?? 502
        finalURL = response.url?.absoluteString
        let type = (http?.value(forHTTPHeaderField: "Content-Type") ?? "").lowercased()
        if status < 300 && Playlist.isPlaylist(url: finalURL ?? "", contentType: type) {
            playlist = Data()
            completionHandler(.allow)
            return
        }
        var h = "HTTP/1.1 \(status) \(FileServer.reason(status))\r\n"
        for k in ["Content-Type", "Content-Length", "Content-Range", "Accept-Ranges", "Last-Modified", "ETag"] {
            if let v = http?.value(forHTTPHeaderField: k) { h += "\(k): \(v)\r\n" }
        }
        if http?.value(forHTTPHeaderField: "Content-Length") == nil {
            h += "Connection: close\r\n"
            keepAlive = false
        }
        h += "\r\n"
        wroteHead = FileServer.write(fd, Data(h.utf8))
        if !wroteHead || head {
            completionHandler(.cancel)
            done.signal()
            return
        }
        completionHandler(.allow)
    }

    func urlSession(_ session: URLSession, dataTask: URLSessionDataTask, didReceive data: Data) {
        if playlist != nil {
            playlist!.append(data)
            if playlist!.count > 4 * 1024 * 1024 { failed = true; dataTask.cancel() }
            return
        }
        if !FileServer.write(fd, data) {
            failed = true
            dataTask.cancel()
        }
    }

    func urlSession(_ session: URLSession, task: URLSessionTask, didCompleteWithError error: Error?) {
        if error != nil && (error as NSError?)?.code != NSURLErrorCancelled { failed = true }
        if !head || !wroteHead { done.signal() }
    }
}

public enum Playlist {
    public static func isPlaylist(url: String, contentType: String) -> Bool {
        if contentType.contains("mpegurl") { return true }
        let path = url.split(whereSeparator: { $0 == "?" || $0 == "#" }).first.map(String.init) ?? url
        return path.lowercased().hasSuffix(".m3u8")
    }

    public static func lastSegment(_ url: String) -> String {
        let path = url.split(whereSeparator: { $0 == "?" || $0 == "#" }).first.map(String.init) ?? url
        let name = path.split(separator: "/").last.map(String.init) ?? ""
        return name.isEmpty ? "x" : name
    }

    /// `rel` against `base`: absolute, scheme-relative, root-relative or relative.
    public static func resolve(base: String, _ rel: String) -> String {
        if let b = URL(string: base), let r = URL(string: rel, relativeTo: b) { return r.absoluteURL.absoluteString }
        return rel
    }

    /// Every link of an HLS playlist (lines that are not tags, and `URI="..."` in tags) made absolute and mapped.
    public static func rewrite(_ text: String, base: String, map: (String) -> String) -> String {
        var out = ""
        for line in text.components(separatedBy: .newlines) {
            let t = line.trimmingCharacters(in: .whitespaces)
            if t.isEmpty { out += "\n"; continue }
            if t.hasPrefix("#") {
                var l = line
                var from = l.startIndex
                while let r = l.range(of: "URI=\"", range: from..<l.endIndex),
                      let close = l[r.upperBound...].firstIndex(of: "\"") {
                    let new = map(resolve(base: base, String(l[r.upperBound..<close])))
                    l.replaceSubrange(r.upperBound..<close, with: new)
                    from = l.index(r.upperBound, offsetBy: new.count + 1, limitedBy: l.endIndex) ?? l.endIndex
                }
                out += l + "\n"
            } else {
                out += map(resolve(base: base, t)) + "\n"
            }
        }
        return out
    }
}
