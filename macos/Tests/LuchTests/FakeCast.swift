// A fake Chromecast for the tests (luch-linux luchd/src/tv/fake_cast.rs): TLS with a self-signed certificate (as a
// real one has), length-prefixed CastMessages, virtual connections that must be opened before anything goes through
// them, a heartbeat that drops a silent sender, the platform's `receiver` namespace (LAUNCH, STOP, GET_STATUS,
// SET_VOLUME) and the Default Media Receiver's `media` namespace (LOAD, PLAY, PAUSE, SEEK, SET_PLAYBACK_RATE,
// EDIT_TRACKS_INFO, GET_STATUS) with mediaSessionId checks. It fetches what it is given like the receiver's page does:
// the video as a <video> would, text tracks with an Origin, refused without CORS.
// SPDX-License-Identifier: GPL-3.0-or-later
import Foundation
import Network
@testable import LuchTV

final class FakeTV {
    var app: String?
    var volume = 0.3
    var muted = false
    var mediaSession = 0
    var content = ""
    var state = "IDLE"
    var idleReason: String?
    var pos = 0.0
    var since: Double?
    var rate = 1.0
    var dur: Double
    var activeTracks: [Int] = []
    /// Text tracks fetched: (content type, had CORS).
    var tracks: [(String, Bool)] = []
    /// What came in: "namespace type".
    var log: [String] = []
    var pingsFromSender = 0
    /// A status to push to the sender now (the TV's remote, the end of the video).
    var push: [String: Any]?

    init(dur: Double) {
        self.dur = dur
    }

    func advance() {
        guard state == "PLAYING" else { return }
        if let s = since {
            pos += (now() - s) * rate
            since = now()
        }
        if pos >= dur {
            pos = dur
            state = "IDLE"
            idleReason = "FINISHED"
            since = nil
            push = mediaStatus(0)
        }
    }

    func receiverStatus(_ request: Int) -> [String: Any] {
        let apps: [[String: Any]] = app.map { s in
            [["appId": CastV2.defaultMediaReceiver, "displayName": "Default Media Receiver", "sessionId": s, "transportId": s,
              "statusText": "Ready To Cast", "isIdleScreen": false,
              "namespaces": [["name": CastV2.nsMedia], ["name": "urn:x-cast:com.google.cast.debugoverlay"]]]]
        } ?? []
        return ["type": "RECEIVER_STATUS", "requestId": request,
                "status": ["applications": apps, "isActiveInput": true, "isStandBy": false,
                           "volume": ["controlType": "attenuation", "level": volume, "muted": muted, "stepInterval": 0.05]]]
    }

    func mediaStatus(_ request: Int) -> [String: Any] {
        guard mediaSession != 0 else { return ["type": "MEDIA_STATUS", "requestId": request, "status": [Any]()] }
        var s: [String: Any] = ["mediaSessionId": mediaSession, "playbackRate": rate, "playerState": state, "currentTime": pos,
                                "supportedMediaCommands": 274447, "activeTrackIds": activeTracks,
                                "volume": ["level": 1, "muted": false],
                                "media": ["contentId": content, "streamType": "BUFFERED", "duration": dur]]
        if let r = idleReason { s["idleReason"] = r }
        return ["type": "MEDIA_STATUS", "requestId": request, "status": [s]]
    }
}

final class FakeCast {
    private(set) var port: UInt16 = 0
    private let lock = NSLock()
    private let tvState: FakeTV
    private let listener: NWListener

    /// Starts one on loopback; a video it is given lasts `dur` seconds.
    init(dur: Double) throws {
        tvState = FakeTV(dur: dur)
        let params = try SelfSigned(names: ["Chromecast"], commonName: "Chromecast").serverParameters()
        params.requiredLocalEndpoint = .hostPort(host: "127.0.0.1", port: .any)
        listener = try NWListener(using: params)
        let ready = DispatchSemaphore(value: 0)
        listener.stateUpdateHandler = { if case .ready = $0 { ready.signal() } }
        listener.newConnectionHandler = { [weak self] c in
            Thread.detachNewThread { self?.connection(ByteStream(c)) }
        }
        listener.start(queue: DispatchQueue(label: "fake.cast"))
        _ = ready.wait(timeout: .now() + 5)
        port = listener.port?.rawValue ?? 0
    }

    deinit {
        listener.cancel()
    }

    func with<T>(_ f: (FakeTV) -> T) -> T {
        lock.lock()
        defer { lock.unlock() }
        return f(tvState)
    }

    /// The TV's remote: pause.
    func pressPause() {
        with { t in
            t.advance()
            t.state = "PAUSED"
            t.since = nil
            t.push = t.mediaStatus(0)
        }
    }

    /// Another phone starts its own app: ours is gone.
    func takeOver() {
        with { t in
            t.app = nil
            t.mediaSession = 0
            var s = t.receiverStatus(0)
            var status = s["status"] as! [String: Any]
            status["applications"] = [["appId": "233637DE", "displayName": "YouTube", "sessionId": "other",
                                       "transportId": "other", "namespaces": [Any]()]]
            s["status"] = status
            t.push = s
        }
    }

    private func connection(_ s: ByteStream) {
        guard (try? s.start(timeout: 5)) != nil else { return }
        let inbox = Mailbox<CastV2.Message?>()
        Thread.detachNewThread {
            while let m = CastV2.readFrame(s) { inbox.put(m) }
            inbox.put(nil)
        }
        var open = Set<String>()
        var lastIn = now()
        var sender = ""
        var pinged = now()
        var tick = now() + 0.1
        while true {
            var out: [CastV2.Message] = []
            if let m = inbox.take(until: tick) {
                guard let m else { return }
                lastIn = now()
                sender = m.source
                out = handle(m, &open)
            } else {
                tick = now() + 0.1
                // A real receiver drops a sender it has not heard from for a while.
                if now() - lastIn > 10 {
                    s.close()
                    return
                }
                if now() - pinged > 1, !sender.isEmpty {
                    pinged = now()
                    out.append(CastV2.Message(CastV2.platform, sender, CastV2.nsHeartbeat, json: ["type": "PING"]))
                }
                let push: ([String: Any], String?)? = with { t in
                    t.advance()
                    defer { t.push = nil }
                    return t.push.map { ($0, t.app) }
                }
                if let (v, app) = push {
                    let media = v["type"] as? String == "MEDIA_STATUS"
                    out.append(CastV2.Message(media ? app ?? "" : CastV2.platform, sender, media ? CastV2.nsMedia : CastV2.nsReceiver, json: v))
                }
            }
            for m in out where !CastV2.writeFrame(s, m) { return }
        }
    }

    /// One message in, the answers out.
    private func handle(_ m: CastV2.Message, _ open: inout Set<String>) -> [CastV2.Message] {
        let v = m.body
        let type = v["type"] as? String ?? ""
        let id = (v["requestId"] as? NSNumber)?.intValue ?? 0
        let (from, to) = (m.source, m.destination)
        let reply = { (ns: String, v: [String: Any]) in [CastV2.Message(to, from, ns, json: v)] }
        with { $0.log.append("\(m.namespace.split(separator: ".").last ?? "") \(type)") }
        switch m.namespace {
        case CastV2.nsConnection:
            if type == "CONNECT" { open.insert(from + ">" + to) }
            if type == "CLOSE" { open.remove(from + ">" + to) }
            return []
        case CastV2.nsHeartbeat:
            guard type == "PING" else { return [] }
            with { $0.pingsFromSender += 1 }
            return reply(CastV2.nsHeartbeat, ["type": "PONG"])
        default: break
        }
        // Nothing goes through a virtual connection that was not opened.
        guard open.contains(from + ">" + to) else { return [] }
        let app = with { $0.app }
        if m.namespace == CastV2.nsReceiver, to == CastV2.platform {
            return with { t in
                switch type {
                case "GET_STATUS": break
                case "LAUNCH" where v["appId"] as? String == CastV2.defaultMediaReceiver:
                    t.app = UUID().uuidString
                    t.mediaSession = 0
                case "LAUNCH":
                    return reply(CastV2.nsReceiver, ["type": "LAUNCH_ERROR", "requestId": id, "reason": "NOT_FOUND"])
                case "STOP":
                    guard v["sessionId"] as? String == t.app else {
                        return reply(CastV2.nsReceiver, ["type": "INVALID_REQUEST", "requestId": id, "reason": "INVALID_SESSION_ID"])
                    }
                    t.app = nil
                    t.mediaSession = 0
                case "SET_VOLUME":
                    let vol = v["volume"] as? [String: Any] ?? [:]
                    if let l = (vol["level"] as? NSNumber)?.doubleValue { t.volume = min(1, max(0, l)) }
                    if let mu = vol["muted"] as? Bool { t.muted = mu }
                default:
                    return reply(CastV2.nsReceiver, ["type": "INVALID_REQUEST", "requestId": id, "reason": "INVALID_COMMAND"])
                }
                return reply(CastV2.nsReceiver, t.receiverStatus(id))
            }
        }
        if m.namespace == CastV2.nsMedia, to == app, let a = media(type, id, v) {
            return reply(CastV2.nsMedia, a)
        }
        return []
    }

    private func media(_ type: String, _ id: Int, _ v: [String: Any]) -> [String: Any]? {
        if type == "LOAD" {
            let media = v["media"] as? [String: Any] ?? [:]
            guard let url = media["contentId"] as? String else { return nil }
            let okStream = ["BUFFERED", "LIVE", "NONE"].contains(media["streamType"] as? String ?? "")
            // The page's <video> asks for the start of the file.
            let video = testFetch(url, headers: ["Range": "bytes=0-"])
            guard okStream, (200..<300).contains(video.0), media["contentType"] is String else {
                return ["type": "LOAD_FAILED", "requestId": id]
            }
            var tracks: [(String, Bool)] = []
            for t in media["tracks"] as? [[String: Any]] ?? [] {
                let r = testFetch(t["trackContentId"] as? String ?? "", headers: ["Origin": "https://www.gstatic.com"])
                // The page parses WebVTT only; a track without CORS is refused by the browser.
                let body = String(decoding: r.2, as: UTF8.self)
                tracks.append((body.hasPrefix("WEBVTT") ? r.1["content-type"] ?? "" : "not WebVTT",
                               r.1["access-control-allow-origin"] != nil))
            }
            return with { t in
                t.tracks = tracks
                t.mediaSession += 1
                t.content = url
                t.pos = (v["currentTime"] as? NSNumber)?.doubleValue ?? 0
                t.rate = 1
                t.idleReason = nil
                t.activeTracks = (v["activeTrackIds"] as? [NSNumber] ?? []).map(\.intValue)
                if v["autoplay"] as? Bool ?? true {
                    t.state = "PLAYING"
                    t.since = now()
                } else {
                    t.state = "PAUSED"
                    t.since = nil
                }
                return t.mediaStatus(id)
            }
        }
        return with { t in
            t.advance()
            if type == "GET_STATUS" { return t.mediaStatus(id) }
            guard (v["mediaSessionId"] as? NSNumber)?.intValue == t.mediaSession, t.mediaSession != 0 else {
                return ["type": "INVALID_REQUEST", "requestId": id, "reason": "INVALID_MEDIA_SESSION_ID"]
            }
            switch type {
            case "PLAY":
                t.state = "PLAYING"
                t.since = now()
            case "PAUSE":
                t.state = "PAUSED"
                t.since = nil
            case "SEEK":
                guard let p = (v["currentTime"] as? NSNumber)?.doubleValue else { return nil }
                t.pos = min(t.dur, max(0, p))
                if t.state == "PLAYING" { t.since = now() }
            case "SET_PLAYBACK_RATE":
                guard let r = (v["playbackRate"] as? NSNumber)?.doubleValue else { return nil }
                t.rate = r
            case "EDIT_TRACKS_INFO":
                guard let ids = v["activeTrackIds"] as? [NSNumber] else { return nil }
                t.activeTracks = ids.map(\.intValue)
            case "STOP":
                t.state = "IDLE"
                t.idleReason = "CANCELLED"
            default:
                return ["type": "INVALID_REQUEST", "requestId": id, "reason": "INVALID_COMMAND"]
            }
            return t.mediaStatus(id)
        }
    }
}
