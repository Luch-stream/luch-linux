// Google Cast (CASTV2) to a Chromecast or a Google TV: TLS on port 8009, each message a 4-byte big-endian length and
// a protobuf `CastMessage` (cast_channel.proto) carrying JSON in a namespace. A sender opens a virtual connection
// (`connection` CONNECT) to the platform (`receiver-0`), keeps it alive (`heartbeat` PING/PONG), launches the Default
// Media Receiver (`receiver` LAUNCH CC1AD845), connects to the app's `transportId` and drives it over `media` (LOAD,
// PLAY, PAUSE, SEEK, MEDIA_STATUS). The same as luchd/src/tv/castv2.rs.
// SPDX-License-Identifier: GPL-3.0-or-later
import Foundation
import LuchCast

public enum CastV2 {
    public static let nsConnection = "urn:x-cast:com.google.cast.tp.connection"
    public static let nsHeartbeat = "urn:x-cast:com.google.cast.tp.heartbeat"
    public static let nsReceiver = "urn:x-cast:com.google.cast.receiver"
    public static let nsMedia = "urn:x-cast:com.google.cast.media"
    public static let platform = "receiver-0"
    public static let defaultMediaReceiver = "CC1AD845"
    static let sender = "sender-luch"
    /// The most a message may be (the receiver's own limit).
    public static let maxMessage = 64 * 1024
    static let pingEvery = 5.0
    /// Silence for this long means the TV is gone (a receiver closes after about the same).
    static let deadAfter = 15.0

    // MARK: - the wire

    public enum Payload: Equatable {
        case text(String)
        case binary(Data)
    }

    /// `CastMessage`: protocol_version = 1 (CASTV2_1_0 = 0), source_id = 2, destination_id = 3, namespace = 4,
    /// payload_type = 5 (STRING = 0, BINARY = 1), payload_utf8 = 6, payload_binary = 7.
    public struct Message: Equatable {
        public var source: String
        public var destination: String
        public var namespace: String
        public var payload: Payload

        public init(source: String, destination: String, namespace: String, payload: Payload) {
            self.source = source
            self.destination = destination
            self.namespace = namespace
            self.payload = payload
        }

        public init(_ source: String, _ destination: String, _ namespace: String, json v: [String: Any]) {
            self.init(source: source, destination: destination, namespace: namespace, payload: .text(LuchTV.json(v)))
        }

        static func varint(_ out: inout Data, _ v: UInt64) {
            var v = v
            while true {
                let b = UInt8(v & 0x7f)
                v >>= 7
                if v == 0 { out.append(b); return }
                out.append(b | 0x80)
            }
        }

        static func bytes(_ out: inout Data, _ field: UInt64, _ b: Data) {
            varint(&out, field << 3 | 2)
            varint(&out, UInt64(b.count))
            out.append(b)
        }

        public func encode() -> Data {
            var out = Data()
            // proto2 `required` fields are always written, even when 0.
            Self.varint(&out, 1 << 3)
            Self.varint(&out, 0)
            Self.bytes(&out, 2, Data(source.utf8))
            Self.bytes(&out, 3, Data(destination.utf8))
            Self.bytes(&out, 4, Data(namespace.utf8))
            switch payload {
            case .text(let t):
                Self.varint(&out, 5 << 3)
                Self.varint(&out, 0)
                Self.bytes(&out, 6, Data(t.utf8))
            case .binary(let b):
                Self.varint(&out, 5 << 3)
                Self.varint(&out, 1)
                Self.bytes(&out, 7, b)
            }
            return out
        }

        public static func decode(_ d: Data) throws -> Message {
            let b = [UInt8](d)
            var i = 0
            func readVarint() -> UInt64? {
                var v: UInt64 = 0
                var shift: UInt64 = 0
                while shift < 64 {
                    guard i < b.count else { return nil }
                    let x = b[i]
                    i += 1
                    v |= UInt64(x & 0x7f) << shift
                    if x & 0x80 == 0 { return v }
                    shift += 7
                }
                return nil
            }
            func bad(_ w: String) -> TVError { TVError("CastMessage: \(w)") }
            var m = Message(source: "", destination: "", namespace: "", payload: .text(""))
            var binary = false
            var text: String?
            var bin: Data?
            while i < b.count {
                guard let key = readVarint() else { throw bad("truncated key") }
                let (field, wire) = (key >> 3, key & 7)
                switch wire {
                case 0:
                    guard let v = readVarint() else { throw bad("truncated varint") }
                    if field == 5 { binary = v == 1 }
                case 2:
                    guard let n = readVarint().map(Int.init), n <= b.count - i else { throw bad("field past the end") }
                    let v = Data(b[i..<i + n])
                    i += n
                    func s() throws -> String {
                        guard let s = String(data: v, encoding: .utf8) else { throw bad("not UTF-8") }
                        return s
                    }
                    switch field {
                    case 2: m.source = try s()
                    case 3: m.destination = try s()
                    case 4: m.namespace = try s()
                    case 6: text = try s()
                    case 7: bin = v
                    default: break
                    }
                case 1:
                    guard b.count - i >= 8 else { throw bad("truncated fixed64") }
                    i += 8
                case 5:
                    guard b.count - i >= 4 else { throw bad("truncated fixed32") }
                    i += 4
                default: throw bad("unknown wire type")
                }
            }
            m.payload = binary ? .binary(bin ?? Data()) : .text(text ?? "")
            return m
        }

        /// The JSON in a text payload ([:] when there is none or it is not JSON).
        public var body: [String: Any] {
            guard case .text(let t) = payload,
                  let v = try? JSONSerialization.jsonObject(with: Data(t.utf8)) as? [String: Any] else { return [:] }
            return v
        }
    }

    public static func writeFrame(_ s: ByteStream, _ m: Message) -> Bool {
        let b = m.encode()
        guard b.count <= maxMessage else { return false }
        var frame = Data()
        withUnsafeBytes(of: UInt32(b.count).bigEndian) { frame.append(contentsOf: $0) }
        frame.append(b)
        return s.write(frame)
    }

    /// The next message; nil at the end of the stream or on garbage.
    public static func readFrame(_ s: ByteStream) -> Message? {
        guard let len = s.read(4) else { return nil }
        let n = Int(len.reduce(UInt32(0)) { $0 << 8 | UInt32($1) })
        guard n <= maxMessage, let b = s.read(n) else { return nil }
        return try? Message.decode(b)
    }

    // MARK: - the channel

    /// What comes to a session's thread: a message from the TV, the end of the connection, or an ask.
    enum Event {
        case message(Message)
        case closed
        case ask(Ask, Reply)
    }

    /// One TLS connection to a Cast device: messages out, messages in (a reader thread puts them in `inbox`).
    final class Channel {
        let stream: ByteStream
        let inbox = Mailbox<Event>()
        private var nextID = 0
        private(set) var lastIn = now()

        init(host: String, port: UInt16) throws {
            stream = try ByteStream.connect(host: host, port: port, parameters: ByteStream.tlsAnyCertificate(), timeout: 5)
            let (s, inbox) = (stream, inbox)
            Thread.detachNewThread {
                while let m = CastV2.readFrame(s) { inbox.put(.message(m)) }
                inbox.put(.closed)
            }
        }

        @discardableResult
        func send(_ to: String, _ ns: String, _ v: [String: Any]) -> Bool {
            writeFrame(stream, Message(sender, to, ns, json: v))
        }

        /// Sends `v` with a new requestId; returns that id (nil when it could not be sent).
        func request(_ to: String, _ ns: String, _ v: [String: Any]) -> Int? {
            nextID += 1
            var v = v
            v["requestId"] = nextID
            return send(to, ns, v) ? nextID : nil
        }

        /// The next event that is not a heartbeat (PINGs are answered here), or nil once `until` has passed.
        func next(until: Double) -> Event? {
            while let e = inbox.take(until: until) {
                guard case .message(let m) = e else { return e }
                lastIn = now()
                if m.namespace == nsHeartbeat {
                    if m.body["type"] as? String == "PING" { send(m.source, nsHeartbeat, ["type": "PONG"]) }
                    continue
                }
                return e
            }
            return nil
        }

        /// The next message, waiting up to `seconds`.
        func message(within seconds: Double) throws -> [String: Any] {
            let until = now() + seconds
            while true {
                switch next(until: until) {
                case .message(let m)?: return m.body
                case .closed?: throw TVError("the Chromecast closed the connection")
                case .ask(_, let r)?: r.send(.failure(TVError("the Chromecast is busy, try again")))
                case nil: throw TVError("the Chromecast did not answer")
                }
            }
        }

        /// Waits up to `seconds` for the answer to request `id`.
        func answer(_ id: Int, within seconds: Double) throws -> [String: Any] {
            let until = now() + seconds
            while true {
                let v = try message(within: until - now())
                if (v["requestId"] as? NSNumber)?.intValue == id { return v }
            }
        }
    }

    // MARK: - playing

    /// What has to be done first for a Chromecast: its page reads only WebVTT, so SubRip files are served as such, and
    /// subtitles it cannot read are left out.
    public static func prepare(_ server: FileServer, _ m: inout Media) {
        m.subs = m.subs.compactMap { s in
            if s.mime.contains("vtt") { return s }
            guard server.asVTT(url: s.url) else { return nil }
            var s = s
            s.mime = "text/vtt"
            return s
        }
    }

    /// The LOAD for `m` in app session `session`.
    public static func load(_ m: Media, session: String) -> [String: Any] {
        var metadata: [String: Any] = ["metadataType": m.photo ? 4 : 0, "title": m.title]
        if let p = m.poster { metadata["images"] = [["url": p]] }
        let tracks: [[String: Any]] = m.subs.enumerated().map { n, s in
            ["trackId": n + 1, "type": "TEXT", "subtype": "SUBTITLES", "trackContentId": s.url,
             "trackContentType": "text/vtt", "language": s.lang, "name": s.lang.isEmpty ? "Subtitles \(n + 1)" : s.lang]
        }
        var media: [String: Any] = ["contentId": m.url, "contentUrl": m.url, "contentType": m.mimeOrDefault,
                                    "streamType": m.photo ? "NONE" : "BUFFERED", "metadata": metadata]
        if !tracks.isEmpty { media["tracks"] = tracks }
        var v: [String: Any] = ["type": "LOAD", "sessionId": session, "media": media, "autoplay": !m.paused,
                                "currentTime": m.start]
        if !m.subs.isEmpty { v["activeTrackIds"] = [1] }
        return v
    }

    /// Our app in a RECEIVER_STATUS: its (sessionId, transportId).
    static func ourApp(_ status: [String: Any]) -> (String, String)? {
        let apps = (status["status"] as? [String: Any])?["applications"] as? [[String: Any]] ?? []
        guard let a = apps.first(where: { $0["appId"] as? String == defaultMediaReceiver }),
              let s = a["sessionId"] as? String, let t = a["transportId"] as? String else { return nil }
        return (s, t)
    }

    /// Launches the Default Media Receiver on `d` and loads `m`; returns once it plays or buffers.
    static func play(_ d: Device, _ m: Media, onState: StateSink?) throws -> TVSession {
        let ch: Channel
        do {
            ch = try Channel(host: d.address, port: d.port)
        } catch {
            throw TVError("\(d.name) (\(d.address):\(d.port)): \(error.localizedDescription)")
        }
        let gone = TVError("the Chromecast went away")
        guard ch.send(platform, nsConnection, ["type": "CONNECT"]),
              let launch = ch.request(platform, nsReceiver, ["type": "LAUNCH", "appId": defaultMediaReceiver]) else {
            ch.stream.close()
            throw gone
        }
        do {
            // The app may take a few seconds to come up; a status without it is not yet the answer.
            let until = now() + 20
            var found: (String, String, [String: Any])?
            while found == nil {
                guard now() < until else { throw TVError("the Chromecast did not start its player") }
                let v = try ch.message(within: until - now())
                switch v["type"] as? String {
                case "LAUNCH_ERROR" where (v["requestId"] as? NSNumber)?.intValue == launch:
                    throw TVError("the Chromecast could not start its player: \(v["reason"] as? String ?? "?")")
                case "RECEIVER_STATUS":
                    if let (s, t) = ourApp(v) { found = (s, t, (v["status"] as? [String: Any])?["volume"] as? [String: Any] ?? [:]) }
                default: break
                }
            }
            let (appSession, transport, volume) = found!
            guard ch.send(transport, nsConnection, ["type": "CONNECT"]),
                  let id = ch.request(transport, nsMedia, load(m, session: appSession)) else { throw gone }
            let first = try ch.answer(id, within: 30)
            guard first["type"] as? String == "MEDIA_STATUS" else {
                throw TVError("the Chromecast could not play it: \(first["reason"] as? String ?? first["type"] as? String ?? "?")")
            }
            let s = TVSession(device: d, onState: onState)
            let p = Player(ch: ch, transport: transport, appSession: appSession, volume: volume, session: s)
            s.noteVolume(p.volumeNow)
            if let why = p.status(first) {
                // Over before it began (a picture shown and done, an error): the session ends with this.
                s.finish(why)
                ch.stream.close()
            } else {
                Thread.detachNewThread { p.run() }
            }
            return s
        } catch {
            ch.stream.close()
            throw error
        }
    }

    final class Player {
        let ch: Channel
        let transport: String
        let appSession: String
        var mediaSession: Int?
        var dur: Double?
        var rate = 1.0
        /// The last state, position and when.
        var last = ("", 0.0, now())
        var volume: [String: Any]
        var pending: [Int: Reply] = [:]
        let session: TVSession

        init(ch: Channel, transport: String, appSession: String, volume: [String: Any], session: TVSession) {
            self.ch = ch
            self.transport = transport
            self.appSession = appSession
            self.volume = volume
            self.session = session
        }

        /// Tells a MEDIA_STATUS as a media_state; the reason when it ends the session.
        func status(_ v: [String: Any]) -> String? {
            let tell = session.tell
            guard let s = (v["status"] as? [[String: Any]])?.first else {
                // Nothing loaded any more: stopped on the TV (or by another phone).
                return mediaSession != nil ? "receiver" : nil
            }
            if let id = (s["mediaSessionId"] as? NSNumber)?.intValue { mediaSession = id }
            let media = s["media"] as? [String: Any] ?? [:]
            if let d = (media["duration"] as? NSNumber)?.doubleValue { dur = d.isFinite && d > 0 ? d : nil }
            if media["streamType"] as? String == "LIVE" { dur = nil }
            if let r = (s["playbackRate"] as? NSNumber)?.doubleValue { rate = r }
            let pos = (s["currentTime"] as? NSNumber)?.doubleValue ?? last.1
            let state: String
            switch s["playerState"] as? String ?? "" {
            case "PLAYING": state = "playing"
            case "PAUSED": state = "paused"
            case "BUFFERING", "LOADING": state = "loading"
            case "IDLE":
                switch s["idleReason"] as? String {
                case "FINISHED"?:
                    tell.tell("ended", pos: dur ?? pos, dur: dur, rate: rate)
                    return "ended"
                case "ERROR"?:
                    tell.error("the Chromecast could not play it")
                    return "receiver"
                case "INTERRUPTED"?: return "replaced"
                case _?: return "receiver"
                case nil: state = "loading"
                }
            default: return nil
            }
            last = (state, pos, now())
            tell.tell(state, pos: pos, dur: dur, rate: rate)
            return nil
        }

        var posNow: Double {
            last.0 == "playing" ? last.1 + (now() - last.2) * rate : last.1
        }

        /// A media_cmd as the media namespace's request; an error when Cast has no such thing.
        func mediaRequest(_ c: [String: Any]) -> Result<[String: Any], TVError> {
            guard let id = mediaSession else { return .failure(TVError("nothing loaded yet")) }
            let number = { (k: String) in (c[k] as? NSNumber)?.doubleValue }
            var v: [String: Any]
            switch c["do"] as? String ?? "" {
            case "play": v = ["type": "PLAY"]
            case "pause": v = ["type": "PAUSE"]
            case "toggle": v = ["type": last.0 == "playing" ? "PAUSE" : "PLAY"]
            case "seek":
                guard let p = number("pos") else { return .failure(TVError("seek needs pos")) }
                v = ["type": "SEEK", "currentTime": max(0, p)]
            case "skip":
                guard let by = number("by") else { return .failure(TVError("skip needs by")) }
                v = ["type": "SEEK", "currentTime": max(0, posNow + by)]
            case "rate":
                guard let r = number("rate") else { return .failure(TVError("rate needs rate")) }
                v = ["type": "SET_PLAYBACK_RATE", "playbackRate": r]
            case "sub":
                let n = (c["id"] as? String).flatMap { $0.hasPrefix("s") ? Int($0.dropFirst()) : nil }
                v = ["type": "EDIT_TRACKS_INFO", "activeTrackIds": n.map { [$0 + 1] } ?? []]
            case "audio": return .failure(TVError("choose sound tracks with the TV's remote"))
            case "state": v = ["type": "GET_STATUS"]
            case let d: return .failure(TVError("unknown media command \(d)"))
            }
            v["mediaSessionId"] = id
            return .success(v)
        }

        /// A Luch volume message as SET_VOLUME (level 0..1 on Cast, 0..100 for us).
        func volumeRequest(_ v: [String: Any]) -> [String: Any] {
            let level = (volume["level"] as? NSNumber)?.doubleValue ?? 0.5
            let step = (volume["stepInterval"] as? NSNumber).map(\.doubleValue).flatMap { $0 > 0 ? $0 : nil } ?? 0.05
            let vol: [String: Any]
            if let l = (v["level"] as? NSNumber)?.doubleValue {
                vol = ["level": min(1, max(0, l / 100))]
            } else if let s = (v["step"] as? NSNumber)?.doubleValue {
                vol = ["level": min(1, max(0, level + (s > 0 ? step : s < 0 ? -step : 0)))]
            } else {
                vol = ["muted": v["muted"] as? Bool ?? false]
            }
            return ["type": "SET_VOLUME", "volume": vol]
        }

        var volumeNow: [String: Any] {
            ["level": Int(((volume["level"] as? NSNumber)?.doubleValue ?? 0) * 100 + 0.5), "muted": volume["muted"] as? Bool ?? false]
        }

        func run() {
            // Asks come through the channel's inbox, in order with what the TV says.
            let asks = session.asks
            Thread.detachNewThread { [ch] in
                while !asks.closed && !ch.inbox.closed {
                    if let a = asks.take(until: now() + 1), !ch.inbox.put(.ask(a.0, a.1)) {
                        a.1.send(.failure(TVError("the Chromecast is no longer playing")))
                    }
                }
            }
            var nextPing = now() + pingEvery
            let why: String = {
                while true {
                    guard let e = ch.next(until: nextPing) else {
                        if now() - ch.lastIn > deadAfter { return "gone" }
                        if !ch.send(platform, nsHeartbeat, ["type": "PING"]) { return "gone" }
                        nextPing = now() + pingEvery
                        continue
                    }
                    switch e {
                    case .closed: return "gone"
                    case .ask(let a, let reply):
                        let req: Result<(String, String, [String: Any]), TVError>
                        switch a {
                        case .leave:
                            reply.send(.success([:]))
                            return "replaced"
                        case .stop:
                            stop()
                            reply.send(.success([:]))
                            return "sender"
                        case .cmd(let c):
                            session.tell.asked()
                            req = mediaRequest(c).map { (transport, nsMedia, $0) }
                        case .volume(let v):
                            req = .success((platform, nsReceiver, volumeRequest(v)))
                        }
                        switch req {
                        case .failure(let e): reply.send(.failure(e))
                        case .success(let (to, ns, r)):
                            guard let id = ch.request(to, ns, r) else {
                                reply.send(.failure(TVError("the Chromecast went away")))
                                return "gone"
                            }
                            pending[id] = reply
                        }
                    case .message(let m):
                        let v = m.body
                        let answer = (v["requestId"] as? NSNumber).map(\.intValue).flatMap { $0 != 0 ? pending.removeValue(forKey: $0) : nil }
                        let type = v["type"] as? String ?? ""
                        switch type {
                        case "MEDIA_STATUS":
                            answer?.send(.success([:]))
                            if let why = status(v) { return why }
                        case "RECEIVER_STATUS":
                            if let vol = (v["status"] as? [String: Any])?["volume"] as? [String: Any] { volume = vol }
                            answer?.send(.success(volumeNow))
                            // Another app took the TV, or ours was closed there.
                            if CastV2.ourApp(v)?.0 != appSession { return "replaced" }
                        case "CLOSE" where m.namespace == nsConnection:
                            return "receiver"
                        default:
                            answer?.send(.failure(TVError(v["reason"] as? String ?? type)))
                        }
                    }
                }
            }()
            for (_, r) in pending { r.send(.failure(TVError("the Chromecast is no longer playing"))) }
            for case .ask(_, let r) in ch.inbox.close() { r.send(.failure(TVError("the Chromecast is no longer playing"))) }
            ch.stream.close()
            session.finish(why)
        }

        /// Closes our app on the TV (it goes back to its home screen) and our connections.
        func stop() {
            if let id = ch.request(platform, nsReceiver, ["type": "STOP", "sessionId": appSession]) {
                _ = try? ch.answer(id, within: 3)
            }
            ch.send(transport, nsConnection, ["type": "CLOSE"])
            ch.send(platform, nsConnection, ["type": "CLOSE"])
        }
    }
}
