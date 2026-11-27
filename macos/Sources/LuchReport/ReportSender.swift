// "Send to the Developers": a report made by "Report a Problem…" goes, at the person's press, to the developers'
// GlitchTip (a Sentry-compatible server) as a Sentry envelope with one event (what happened, about.txt and the tails
// of the logs, as hidden as the file) and, best effort, a second with the zip attached. No account: the DSN's key is
// baked into the build (Info.plist LuchReportDSN) and only says "this is Luch". As luchd's report_send.rs.
// SPDX-License-Identifier: GPL-3.0-or-later
import Foundation
import LuchProto

public enum ReportSender {
    /// The error of a send the person stopped.
    public static let stopped = "stopped"
    /// Each text in the event at most (its tail), and all of them: GlitchTip takes events up to 1 MB.
    static let each = 64 * 1024
    static let total = 700 * 1024

    /// Where a DSN says events go.
    public struct DSN: Equatable {
        public let key: String
        /// `<scheme>://<host>/<path>/api/<project>/envelope/`.
        public let url: URL

        /// `https://<public_key>@<host>[:port][/path]/<project_id>`.
        public init?(_ dsn: String) {
            guard let c = URLComponents(string: dsn.trimmingCharacters(in: .whitespaces)),
                  c.scheme == "https" || c.scheme == "http", let key = c.user, !key.isEmpty,
                  let host = c.host, !host.isEmpty else { return nil }
            var path = c.path.split(separator: "/").map(String.init)
            guard let project = path.popLast(), !project.isEmpty, project.allSatisfy(\.isASCII),
                  project.allSatisfy(\.isNumber) else { return nil }
            var out = URLComponents()
            out.scheme = c.scheme
            out.host = host
            out.port = c.port
            out.path = "/" + (path + ["api", project, "envelope", ""]).joined(separator: "/")
            guard let url = out.url else { return nil }
            self.key = key
            self.url = url
        }
    }

    /// The DSN to send to: `LUCH_REPORT_DSN` in the environment when set, else the app's Info.plist `LuchReportDSN`;
    /// nil hides the button.
    public static func configured(info: [String: Any]? = Bundle.main.infoDictionary,
                                  env: [String: String] = ProcessInfo.processInfo.environment) -> DSN? {
        let d = env["LUCH_REPORT_DSN"] ?? info?["LuchReportDSN"] as? String ?? ""
        return d.trimmingCharacters(in: .whitespaces).isEmpty ? nil : DSN(d)
    }

    /// "LUCH-" and the event id's first 8 hex digits: what the person tells in a chat, and finds the event by.
    public static func number(_ eventID: String) -> String {
        "LUCH-" + eventID.prefix(8).uppercased()
    }

    /// A Sentry envelope: the header line, then each item's header (with its payload's `length`) and payload.
    public static func envelope(header: [String: Any], items: [([String: Any], Data)]) -> Data {
        var out = line(header)
        for (h, payload) in items {
            var h = h
            h["length"] = payload.count
            out += line(h) + payload + Data("\n".utf8)
        }
        return out
    }

    private static func line(_ v: [String: Any]) -> Data {
        ((try? JSONSerialization.data(withJSONObject: v, options: [.sortedKeys, .withoutEscapingSlashes])) ?? Data())
            + Data("\n".utf8)
    }

    /// `2026-01-02T03:04:05Z`.
    public static func utc(_ d: Date) -> String {
        let f = ISO8601DateFormatter()
        f.formatOptions = [.withInternetDateTime]
        return f.string(from: d)
    }

    /// At most `max` bytes of `s`'s end, from a character's start.
    static func tail(_ s: String, _ max: Int) -> String {
        let u = Array(s.utf8)
        guard u.count > max else { return s }
        var i = u.count - max
        while i < u.count, u[i] & 0xC0 == 0x80 { i += 1 }
        return String(decoding: u[i...], as: UTF8.self)
    }

    /// A tag's value: one line, 200 characters at most (Sentry's limit).
    static func tag(_ s: String) -> String {
        String(s.split(whereSeparator: \.isWhitespace).joined(separator: " ").prefix(200))
    }

    /// The event for a report: its title is what happened's first line, its `extra` the texts of the file (tails).
    public static func event(_ f: ProblemReport.Facts, id: String, now: Date) -> [String: Any] {
        let what = f.what.trimmingCharacters(in: .whitespacesAndNewlines)
        let first = what.split(separator: "\n").first.map { String($0.trimmingCharacters(in: .whitespaces).prefix(100)) } ?? ""
        var tags: [String: String] = [:]
        for (k, v) in [("app", f.app), ("os", f.os), ("receivers", f.receivers.joined(separator: "; ")), ("codec", f.codec)]
            where !v.isEmpty {
            tags[k] = tag(v)
        }
        var left = total
        var extra: [String: String] = [:]
        let w = tail(what, each)
        left -= w.utf8.count
        extra["what happened"] = w
        for (name, text) in f.texts {
            let t = tail(text, min(each, left))
            left -= t.utf8.count
            extra[name] = t
        }
        return [
            "event_id": id,
            "timestamp": Int(now.timeIntervalSince1970),
            "level": "info",
            "platform": "other",
            "logger": "luch.report",
            "message": first.isEmpty ? "Problem report" : "Problem report: \(first)",
            "release": "luch-macos@\(f.version)",
            "tags": tags,
            "contexts": ["os": ["name": "macOS", "version": f.os.replacingOccurrences(of: "macOS ", with: "")],
                         "device": ["model": f.model]],
            "extra": extra,
        ]
    }

    /// What the server answered, for the person: nil on a 2xx, else why not.
    public static func refusal(status: Int, body: Data, retryAfter: String?) -> String? {
        if (200..<300).contains(status) { return nil }
        let json = (try? JSONSerialization.jsonObject(with: body)) as? [String: Any]
        let detail = (json?["detail"] as? String) ?? (json?["error"] as? String)
            ?? (json == nil ? tag(String(decoding: body, as: UTF8.self)) : "")
        switch status {
        case 429:
            let wait: String
            switch retryAfter.flatMap({ Int($0.trimmingCharacters(in: .whitespaces)) }) {
            case let s? where s >= 90: wait = "in \((s + 59) / 60) min"
            case let s?: wait = "in \(s) s"
            case nil: wait = "later"
            }
            return "the developers' server takes no more reports now, try again \(wait)"
        case 401, 403: return "the developers' server did not take this build's key (HTTP \(status))"
        case 413: return "the report is too big for the developers' server"
        default:
            return detail.isEmpty ? "the developers' server answered HTTP \(status)"
                : "the developers' server answered HTTP \(status): \(detail)"
        }
    }

    /// A report sent: the number to tell, and why the zip did not go when it did not.
    public struct Sent {
        public let number: String
        public let eventID: String
        public let attachError: String?
    }

    /// One send: the event, then the zip, with the bytes sent so far for a progress bar. Cancel stops it.
    public final class Upload: NSObject, URLSessionTaskDelegate {
        private let report: ProblemReport.Result
        private let dsn: DSN
        private let client: String
        private var session: URLSession!
        private var task: URLSessionTask?
        private var done = 0
        private var total = 0
        private var progress: (Double) -> Void = { _ in }
        private let lock = NSLock()
        private var cancelled = false

        public init(_ report: ProblemReport.Result, to dsn: DSN) {
            self.report = report
            self.dsn = dsn
            client = "luch/\(report.facts.version)"
            super.init()
            let c = URLSessionConfiguration.ephemeral
            c.timeoutIntervalForRequest = 60
            session = URLSession(configuration: c, delegate: self, delegateQueue: nil)
        }

        /// Runs it; `progress` (0…1) and `finished` on a background queue.
        public func start(progress: @escaping (Double) -> Void,
                          finished: @escaping (Swift.Result<Sent, ReportError>) -> Void) {
            self.progress = progress
            // The session holds its delegate until it is let go.
            let finished = { [session] (r: Swift.Result<Sent, ReportError>) in
                session?.finishTasksAndInvalidate()
                finished(r)
            }
            let zip: Data
            do {
                zip = try Data(contentsOf: report.file)
            } catch {
                finished(.failure(ReportError("cannot read \(report.file.lastPathComponent): \(error.localizedDescription)")))
                return
            }
            let id = UUID().uuidString.replacingOccurrences(of: "-", with: "").lowercased()
            let now = Date()
            let header: [String: Any] = ["event_id": id, "sent_at": ReportSender.utc(now)]
            let ev = (try? JSONSerialization.data(withJSONObject: ReportSender.event(report.facts, id: id, now: now),
                                                  options: [.sortedKeys, .withoutEscapingSlashes])) ?? Data()
            let first = ReportSender.envelope(header: header, items: [(["type": "event"], ev)])
            let second = ReportSender.envelope(header: header, items: [(["type": "attachment", "filename": report.file.lastPathComponent,
                                                            "content_type": "application/zip",
                                                            "attachment_type": "event.attachment"], zip)])
            total = first.count + second.count
            post(first) { [self] why in
                if let why { return finished(.failure(ReportError(why))) }
                done = first.count
                // GlitchTip may refuse attachments (glitchtip-backend #241): the event is in, so this cannot fail it.
                post(second) { why in
                    if let why { log("report \(ReportSender.number(id)): the zip was not taken: \(why)") }
                    finished(.success(Sent(number: ReportSender.number(id), eventID: id, attachError: why)))
                }
            }
        }

        public func cancel() {
            lock.lock()
            cancelled = true
            let t = task
            lock.unlock()
            t?.cancel()
        }

        private func post(_ body: Data, then: @escaping (String?) -> Void) {
            var r = URLRequest(url: dsn.url)
            r.httpMethod = "POST"
            r.setValue("Sentry sentry_version=7, sentry_key=\(dsn.key), sentry_client=\(client)",
                       forHTTPHeaderField: "X-Sentry-Auth")
            r.setValue("application/x-sentry-envelope", forHTTPHeaderField: "Content-Type")
            let t = session.uploadTask(with: r, from: body) { data, response, error in
                if let error {
                    let stopped = (error as? URLError)?.code == .cancelled
                    return then(stopped ? ReportSender.stopped : "cannot reach the developers' server: \(error.localizedDescription)")
                }
                let h = response as? HTTPURLResponse
                then(ReportSender.refusal(status: h?.statusCode ?? 0, body: data ?? Data(),
                             retryAfter: h?.value(forHTTPHeaderField: "Retry-After")))
            }
            lock.lock()
            task = t
            let stop = cancelled
            lock.unlock()
            if stop { return then(ReportSender.stopped) }
            t.resume()
        }

        public func urlSession(_ session: URLSession, task: URLSessionTask, didSendBodyData bytesSent: Int64,
                               totalBytesSent: Int64, totalBytesExpectedToSend: Int64) {
            guard total > 0 else { return }
            progress(min(1, Double(done + Int(totalBytesSent)) / Double(total)))
        }
    }

    /// The whole send, waited for (luchctl's `report --send`, answered on the socket's thread).
    public static func send(_ report: ProblemReport.Result, to dsn: DSN) -> Swift.Result<Sent, ReportError> {
        let wait = DispatchSemaphore(value: 0)
        var out: Swift.Result<Sent, ReportError> = .failure(ReportError(ReportSender.stopped))
        let u = Upload(report, to: dsn)
        u.start(progress: { _ in }) { r in
            out = r
            wait.signal()
        }
        wait.wait()
        return out
    }
}
