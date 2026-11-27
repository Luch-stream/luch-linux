// The local API's cast commands (luchctl, the browser extension's host): one CastSession per receiver and one file
// server for all of them. Runs on the local socket's thread: these calls wait for the receiver.
// SPDX-License-Identifier: GPL-3.0-or-later
import Foundation
import LuchCast
import LuchNet
import LuchProto

final class Caster {
    static let commands: Set<String> = ["cast", "media_cmd", "media_state", "stop_cast", "sleep", "volume", "page", "receiver_modes"]

    private let lock = NSLock()
    private var sessions: [String: CastSession] = [:]
    private var server: FileServer?

    /// `mirroring`: the receiver the app mirrors or plays sound to, which counts as being shown on.
    private func find(_ request: [String: Any], in receivers: [Receiver], mirroring: String?) -> Receiver? {
        let wanted = (request["id"] as? String) ?? (request["name"] as? String)
        if let wanted { return receivers.first { $0.id == wanted || $0.name == wanted } }
        if receivers.count == 1 { return receivers[0] }
        // Unnamed with several around: the one something is being shown on, when that is only one.
        lock.lock()
        let showing = receivers.filter { sessions[$0.id]?.isOpen == true || $0.id == mirroring }
        lock.unlock()
        return showing.count == 1 ? showing[0] : nil
    }

    private func session(_ r: Receiver) throws -> CastSession {
        lock.lock()
        if let s = sessions[r.id], s.isOpen {
            lock.unlock()
            return s
        }
        lock.unlock()
        let s = try CastSession.open(r)
        lock.lock()
        sessions[r.id] = s
        lock.unlock()
        return s
    }

    private func files() throws -> FileServer {
        lock.lock()
        defer { lock.unlock() }
        if let server { return server }
        let s = FileServer()
        try s.start()
        server = s
        return s
    }

    /// A link for the cover of what plays (now_playing's `art`), served like any cast file to `client` only.
    func shareArt(_ file: URL, client: String, me: String) -> String? {
        guard let server = try? files(), let head = try? FileHandle(forReadingFrom: file).read(upToCount: 4) else {
            return nil
        }
        let mime = head.starts(with: [0x89, 0x50, 0x4e, 0x47]) ? "image/png" : "image/jpeg"
        let token = server.share(.file(file), client: client, mime: mime)
        return server.url(host: me, token: token, name: mime == "image/png" ? "cover.png" : "cover.jpg")
    }

    func handle(_ request: [String: Any], receivers: [Receiver], mirroring: String? = nil) -> [String: Any] {
        guard let r = find(request, in: receivers, mirroring: mirroring) else {
            let names = receivers.map { "\"\($0.name)\"" }.joined(separator: ", ")
            if request["id"] == nil, request["name"] == nil, receivers.count > 1 {
                return ["error": "several receivers, name one with --to: \(names)"]
            }
            return ["error": receivers.isEmpty ? "no receivers found" : "receiver not found; there are \(names)"]
        }
        do {
            switch request["cmd"] as? String ?? "" {
            case "cast":
                let items = request["items"] as? [String] ?? []
                let options = try CastOptions(json: request["options"] as? [String: Any] ?? [:])
                let kind = try session(r).cast(items, options: options, server: try files())
                return ["ok": true, "kind": kind]
            case "media_cmd":
                guard let cmd = request["media"] as? [String: Any], cmd["do"] is String else { return ["error": "no media command"] }
                try session(r).command(cmd)
                return ["ok": true]
            case "media_state":
                lock.lock()
                let s = sessions[r.id]
                lock.unlock()
                return ["ok": true, "state": s?.state ?? [:]]
            case "stop_cast":
                lock.lock()
                let s = sessions[r.id]
                lock.unlock()
                s?.stop()
                server?.forget(client: r.address)
                return ["ok": true]
            case "sleep":
                try session(r).sleep()
                return ["ok": true]
            case "volume":
                return ["ok": true, "volume": try session(r).volume(request["what"] as? String ?? "")]
            case "page":
                return ["ok": true, "page": try session(r).page(request["what"] as? String ?? "next")]
            case "receiver_modes":
                return ["ok": true, "modes": try session(r).welcome["modes"] ?? []]
            default:
                return ["error": "unknown command"]
            }
        } catch {
            return ["error": error.localizedDescription]
        }
    }
}
