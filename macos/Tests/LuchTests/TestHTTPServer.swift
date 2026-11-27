// A tiny HTTP/1.1 server on loopback for the fakes: keep-alive, Content-Length bodies, one thread per connection.
// SPDX-License-Identifier: GPL-3.0-or-later
import Darwin
import Foundation

struct TestRequest {
    var method: String
    var path: String
    /// Names lowercased.
    var headers: [String: String]
    var body: Data
}

struct TestResponse {
    var status: Int
    var headers: [(String, String)] = []
    var body = Data()
}

final class TestHTTPServer {
    let port: UInt16
    private let fd: Int32
    private let handler: (TestRequest) -> TestResponse

    init(_ handler: @escaping (TestRequest) -> TestResponse) {
        self.handler = handler
        let fd = socket(AF_INET, SOCK_STREAM, 0)
        var yes: Int32 = 1
        setsockopt(fd, SOL_SOCKET, SO_REUSEADDR, &yes, socklen_t(MemoryLayout<Int32>.size))
        var a = sockaddr_in()
        a.sin_len = UInt8(MemoryLayout<sockaddr_in>.size)
        a.sin_family = sa_family_t(AF_INET)
        inet_pton(AF_INET, "127.0.0.1", &a.sin_addr)
        _ = withUnsafePointer(to: &a) { $0.withMemoryRebound(to: sockaddr.self, capacity: 1) { bind(fd, $0, socklen_t(MemoryLayout<sockaddr_in>.size)) } }
        listen(fd, 16)
        var len = socklen_t(MemoryLayout<sockaddr_in>.size)
        _ = withUnsafeMutablePointer(to: &a) { $0.withMemoryRebound(to: sockaddr.self, capacity: 1) { getsockname(fd, $0, &len) } }
        self.fd = fd
        port = UInt16(bigEndian: a.sin_port)
        let handler = handler
        Thread.detachNewThread {
            while true {
                let c = accept(fd, nil, nil)
                if c < 0 { return }
                var one: Int32 = 1
                setsockopt(c, SOL_SOCKET, SO_NOSIGPIPE, &one, socklen_t(MemoryLayout<Int32>.size))
                Thread.detachNewThread { TestHTTPServer.serve(c, handler); close(c) }
            }
        }
    }

    deinit {
        close(fd)
    }

    private static func serve(_ c: Int32, _ handler: (TestRequest) -> TestResponse) {
        var carry = Data()
        func fill() -> Bool {
            var chunk = [UInt8](repeating: 0, count: 4096)
            let n = recv(c, &chunk, chunk.count, 0)
            if n <= 0 { return false }
            carry.append(contentsOf: chunk[0..<n])
            return true
        }
        while true {
            while carry.range(of: Data("\r\n\r\n".utf8)) == nil { if !fill() { return } }
            let end = carry.range(of: Data("\r\n\r\n".utf8))!
            let head = String(decoding: carry[carry.startIndex..<end.lowerBound], as: UTF8.self)
            carry.removeSubrange(carry.startIndex..<end.upperBound)
            var lines = head.components(separatedBy: "\r\n")
            let first = lines.removeFirst().split(separator: " ").map(String.init)
            var headers: [String: String] = [:]
            for l in lines {
                guard let colon = l.firstIndex(of: ":") else { continue }
                headers[l[..<colon].trimmingCharacters(in: .whitespaces).lowercased()] = l[l.index(after: colon)...].trimmingCharacters(in: .whitespaces)
            }
            let len = Int(headers["content-length"] ?? "0") ?? 0
            while carry.count < len { if !fill() { return } }
            let body = carry.prefix(len)
            carry.removeFirst(len)
            let r = handler(TestRequest(method: first.first ?? "", path: first.count > 1 ? first[1] : "/", headers: headers, body: Data(body)))
            var out = "HTTP/1.1 \(r.status) X\r\n"
            for (k, v) in r.headers { out += "\(k): \(v)\r\n" }
            out += "Content-Length: \(r.body.count)\r\n\r\n"
            let data = Data(out.utf8) + r.body
            let sent = data.withUnsafeBytes { send(c, $0.baseAddress, $0.count, 0) }
            if sent != data.count { return }
        }
    }
}

/// A blocking GET with headers, as a TV makes it: (status, headers lowercased, body).
func testFetch(_ url: String, headers: [String: String] = [:], method: String = "GET") -> (Int, [String: String], Data) {
    var r = URLRequest(url: URL(string: url)!, timeoutInterval: 5)
    r.httpMethod = method
    for (k, v) in headers { r.setValue(v, forHTTPHeaderField: k) }
    let done = DispatchSemaphore(value: 0)
    var out: (Int, [String: String], Data) = (0, [:], Data())
    URLSession(configuration: .ephemeral).dataTask(with: r) { data, response, _ in
        let h = response as? HTTPURLResponse
        var hs: [String: String] = [:]
        for (k, v) in h?.allHeaderFields ?? [:] { hs[String(describing: k).lowercased()] = String(describing: v) }
        out = (h?.statusCode ?? 0, hs, data ?? Data())
        done.signal()
    }.resume()
    _ = done.wait(timeout: .now() + 6)
    return out
}
