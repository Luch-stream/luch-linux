// A blocking HTTP call for the threads that drive TVs (descriptions, SOAP): they wait for the answer anyway.
// SPDX-License-Identifier: GPL-3.0-or-later
import Foundation

struct HTTPAnswer {
    var status: Int
    var headers: [String: String]
    var body: Data
    var text: String { String(decoding: body, as: UTF8.self) }
}

enum HTTP {
    private static let session: URLSession = {
        let c = URLSessionConfiguration.ephemeral
        c.requestCachePolicy = .reloadIgnoringLocalCacheData
        c.httpShouldUsePipelining = false
        return URLSession(configuration: c)
    }()

    /// The answer, or the reason there is none (no route, a timeout).
    static func call(_ url: String, method: String = "GET", headers: [String: String] = [:], body: Data? = nil,
                     timeout: Double) -> Result<HTTPAnswer, TVError> {
        guard let u = URL(string: url) else { return .failure(TVError("bad link \(url)")) }
        var r = URLRequest(url: u, timeoutInterval: timeout)
        r.httpMethod = method
        r.httpBody = body
        for (k, v) in headers { r.setValue(v, forHTTPHeaderField: k) }
        let done = DispatchSemaphore(value: 0)
        var out: Result<HTTPAnswer, TVError> = .failure(TVError("no answer"))
        session.dataTask(with: r) { data, response, error in
            if let h = response as? HTTPURLResponse {
                var hs: [String: String] = [:]
                for (k, v) in h.allHeaderFields { hs[String(describing: k).lowercased()] = String(describing: v) }
                out = .success(HTTPAnswer(status: h.statusCode, headers: hs, body: data ?? Data()))
            } else {
                out = .failure(TVError(error?.localizedDescription ?? "no answer"))
            }
            done.signal()
        }.resume()
        _ = done.wait(timeout: .now() + timeout + 1)
        return out
    }
}
